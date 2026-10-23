# AgentSight

[中文版](README_zh.md)

eBPF-based observability tool for AI Agents on Linux, providing zero-intrusion monitoring of LLM API calls, token consumption, process behavior, and SSL/TLS traffic. AgentSight is an observability component of [ANOLISA](../../README.md).

> **macOS support**: On macOS, AgentSight compiles with two commands — `agentsight trace` (trajectory collector, scans local JSONL session files → ATIF → SQLite, no eBPF) and `agentsight serve` (Dashboard UI + trajectory viewer). The same source tree produces a full-featured eBPF binary on Linux and a trajectory-only binary on macOS via OS-conditional compilation.

## Features

- **Zero-Intrusion Monitoring** — eBPF kernel probes capture events without modifying agent code or configurations.
- **SSL/TLS Traffic Decryption** — uprobe-based interception of OpenSSL/GnuTLS library calls to capture plaintext HTTP traffic.
- **LLM Token Accounting** — Precise token counting with Hugging Face tokenizer support (Qwen series and more).
- **AI Agent Auto-Discovery** — Scans `/proc` and monitors `execve` events to dynamically detect running AI agent processes.
- **Streaming Response Support** — Parses Server-Sent Events (SSE) for tracking streamed LLM responses.
- **Audit Logging** — Complete audit trail of LLM calls and process operations with structured records.
- **Cloud Integration** — Native export to Alibaba Cloud SLS (Simple Log Service) for centralized log analysis.
- **GenAI Semantic Events** — Builds structured semantic events for LLM calls, tool usage, and agent interactions.

## Architecture

AgentSight operates a unified data pipeline:

```
┌──────────┐    ┌────────┐    ┌────────────┐    ┌──────────┐    ┌───────┐    ┌─────────┐
│  Probes  │───▶│ Parser │───▶│ Aggregator │───▶│ Analyzer │───▶│ GenAI │───▶│ Storage │
└──────────┘    └────────┘    └────────────┘    └──────────┘    └───────┘    └─────────┘
  eBPF events    HTTP/SSE      Req-Resp          Token/Audit     Semantic     SQLite /
  (kernel)       extraction    correlation       extraction      events       SLS export
```

| Stage | Description |
|-------|-------------|
| **Probes** | eBPF programs (sslsniff, proctrace, procmon) capture kernel events via ring buffer |
| **Parser** | Extracts structured HTTP messages, SSE events, and process exec data |
| **Aggregator** | Correlates request-response pairs; tracks process lifecycle via LRU cache |
| **Analyzer** | Produces audit records, token usage stats, and LLM API messages |
| **GenAI** | Transforms results into semantic events (LLM calls, tool use, agent interactions) |
| **Storage** | Opens typed SQLite Stores through `DatabaseManager`, applies schema-aware lifecycle policies, and optionally exports to Alibaba Cloud SLS |

### eBPF Probes

| Probe | Source | Description |
|-------|--------|-------------|
| **sslsniff** | `src/bpf/sslsniff.bpf.c` | uprobe on SSL_read/SSL_write to capture plaintext from encrypted connections |
| **proctrace** | `src/bpf/proctrace.bpf.c` | Traces execve syscalls, captures command-line args, builds process tree |
| **procmon** | `src/bpf/procmon.bpf.c` | Lightweight process monitor for creation/exit events (agent discovery) |

### Project Structure

```
agentsight/
├── src/
│   ├── bpf/            # eBPF C programs (sslsniff, proctrace, procmon)
│   ├── probes/         # eBPF probe management and event polling
│   ├── parser/         # HTTP, SSE, and process event parsers
│   ├── aggregator/     # Request-response correlation and process aggregation
│   ├── analyzer/       # Token extraction, audit records, message parsing
│   ├── genai/          # GenAI semantic event builder and SLS uploader
│   ├── storage/        # SQLite-backed stores (audit, token, HTTP, GenAI)
│   ├── discovery/      # AI agent process scanner (/proc + eBPF)
│   ├── tokenizer/      # HuggingFace tokenizer integration for token counting
│   ├── local/          # macOS-only: trajectory viewer server + collector dispatch
│   ├── bin/            # CLI entry points (agentsight, cli subcommands)
│   ├── unified.rs      # Main pipeline orchestrator
│   ├── database.rs     # Typed Store registry and maintenance ownership
│   ├── config.rs       # Unified configuration management
│   └── event.rs        # Unified event type definitions
├── crates/
│   └── agentsight-sqlite-lifecycle/ # Business-model-free SQLite lifecycle primitives
├── Cargo.toml
├── build.rs            # eBPF skeleton generation for three probes
└── agentsight.spec     # RPM packaging spec
```

## CLI Commands

> Commands `token`, `audit`, `discover`, `metrics`, `interruption`, `skill-metrics`, and `summary` require Linux eBPF and are not available on macOS. `trace` and `serve` work cross-platform: on Linux `trace` runs the full eBPF pipeline, on macOS it runs the trajectory collector only (no eBPF).

### `agentsight trace`

Start tracing of AI agent activity.

**Linux**: Full eBPF-based tracing (probes → parser → aggregator → storage). Also runs trajectory collector if `features.trajectory_collection.enabled` is set.

**Linux without privileges**: `--no-ebpf` skips the probes and runs the trajectory collector alone, so unprivileged sandboxes and containers still collect trajectories. The collector supports Claude Agent SDK/Claude Code, Qoder, Codex, and Cursor JSONL sessions plus Hermes `state.db`; expose a non-standard SDK config through `CLAUDE_CONFIG_DIR`, or supply per-tenant workspace roots through `features.trajectory_collection.scan_dirs`. The flag implies trajectory collection regardless of `features.trajectory_collection.enabled`, since it is the only remaining data source. Data derived from eBPF — token metering, audit events, interruption detection — is unavailable in this mode.

**macOS**: Trajectory collection only — scans local JSONL session files (Claude Code, Qoder, Codex, Cursor), converts to ATIF v1.7, and stores in `trajectories.db`. No eBPF.

```bash
# Foreground mode
sudo agentsight trace

# Trajectory collection only — no root, no CAP_BPF required
agentsight trace --no-ebpf

# Daemon mode with SLS export
sudo agentsight trace --daemon \
  --sls-endpoint <endpoint> \
  --sls-project <project> \
  --sls-logstore <logstore>
```

> With `--no-ebpf`, `trajectories.db` is written to the shared data directory when it is writable, otherwise to `$HOME/.local/share/agentsight/`. The startup output prints the resolved path and the matching `serve --db` command.

### `agentsight token`

Query token consumption data.

When the Linux systemd service owns the data, run these queries with `sudo`:

```bash
# Today's token usage
sudo agentsight token

# This week, compared to last week
sudo agentsight token --period week --compare

# Detailed breakdown by role and type
sudo agentsight token --detail

# JSON output
sudo agentsight token --json
```

### `agentsight audit`

Query audit events (LLM calls, process operations).

```bash
# Recent audit events
agentsight audit

# Filter by PID and event type
agentsight audit --pid 12345 --type llm

# Summary statistics
agentsight audit --summary

# Read a database copied from another host
agentsight audit --db /backup/agentsight/agentsight.db
```

### `agentsight serve`

Start the HTTP API server and serve the embedded Dashboard UI.

> **macOS**: Reads from `trajectories.db` (populated by `agentsight trace`). The `--db` and `--config` flags are Linux-only.

```bash
# Start with default settings (binds to 127.0.0.1:7396)
agentsight serve

# Bind to all interfaces on a custom port
agentsight serve --host 0.0.0.0 --port 8080

# Point to a specific database file
agentsight serve --db /path/to/genai_events.db
```

### `agentsight discover`

Discover AI agents running on the system.

```bash
# Scan for running agents
agentsight discover

# List all known agent types
agentsight discover --list-known

# Verbose output with executable paths
agentsight discover --verbose
```

## Dashboard

The Dashboard is a React-based web UI for visualizing conversation history, trace details, and token statistics. It is embedded into the `agentsight serve` binary at compile time. By default, the Dashboard follows the browser language; you can switch languages manually, and the choice is persisted across refreshes.

### Build the Dashboard

```bash
cd src/agentsight

# Build frontend and embed into frontend-dist/ (required before cargo build)
make build-frontend

# Then build the Rust binary with the embedded UI
make build

# Or do both in one step
make build-all
```

### Scenario 1 — Collect data and view the Dashboard simultaneously

**Linux** (eBPF + trajectory collector):

Run the tracer and the API server in two separate terminals:

```bash
# Stop the packaged tracer before starting a foreground tracer
sudo systemctl stop agentsight.service

# Terminal 1: start eBPF tracing (writes to SQLite)
sudo agentsight trace

# Terminal 2: start the API server (reads from the same SQLite)
sudo agentsight serve
```

**macOS** (trajectory collector only):

```bash
# Terminal 1: start trajectory collection (scans JSONL → trajectories.db)
agentsight trace

# Terminal 2: start the API server (reads from trajectories.db)
agentsight serve
```

Open `http://127.0.0.1:7396` in your browser. The Dashboard auto-refreshes as new data arrives.

> **Running on a remote server?** Bind to all interfaces and access via the server's public IP:
> ```bash
> agentsight serve --host 0.0.0.0 --port 7396
> ```
> Then open `http://<server-public-ip>:7396` in your local browser.
> Make sure port 7396 is allowed in the server's firewall / security group rules.

### Scenario 2 — Browse historical data only

No tracing needed. Just start the server pointing at an existing database:

```bash
agentsight serve --db /path/to/genai_events.db
```

Open `http://127.0.0.1:7396` to explore recorded conversations and traces.

### Dashboard Development

To iterate on the frontend without rebuilding the Rust binary:

```bash
cd src/agentsight/dashboard
npm install
npm run dev          # starts webpack-dev-server on http://localhost:3004
```

When finished, run `make build-frontend && cargo build --release` to embed the updated UI.

### Resource Benchmarking (Linux)

[`scripts/benchmark/single_run/resource_run.sh`](scripts/benchmark/single_run/resource_run.sh)
measures AgentSight RSS and CPU with configurable Agent count and output tok/s,
defaulting to 32 Agents, 100 tok/s each, and 60 seconds. It prints average/peak RSS,
host RAM shares, average/P95/peak CPU (100% = one logical CPU), and load/capture
checks. Cooperative CPU pacing defaults to 25% of one logical CPU with delayed delivery;
`--cpu-budget-percent N` overrides the configured target (`0` disables pacing). Load-window and response-drain statistics are separate. It supports Git A/B comparisons with
`--ab --baseline REF --candidate REF`. `--ring-buffer-mb N` sets the ring buffer
size; reported probe drops invalidate the comparison. Tracing requires root or eBPF capabilities;
the script requests sudo by default. See the [benchmark usage guide](scripts/docs/BENCHMARK_USAGE.md)
for one-command examples, working-tree snapshots and reports.

Add `--ffi` to build and measure a minimal C FFI consumer instead of the CLI.
`--ffi-api typed|v2` selects typed callbacks (default) or JSON envelope callbacks;
`--candidate-lib PATH` and `--baseline-lib PATH` accept prebuilt shared libraries.
The measurement includes AgentSight threads, FFI conversion, receipt forwarding and the small C host baseline.
FFI readiness and capture validation use callback receipts independently of SQLite storage.

## Quick Start

### Prerequisites

#### System Packages

Before building, install the required system packages:

**Anolis OS / CentOS / RHEL:**
```bash
sudo yum install -y openssl-devel elfutils-libelf-devel perl-IPC-Cmd libbpf-devel clang llvm bpftool
```

**Ubuntu / Debian:**
```bash
sudo apt install -y pkg-config libssl-dev libelf-dev libbpf-dev clang llvm linux-tools-common
```

| Package | Required for |
|---------|-------------|
| `openssl-devel` | OpenSSL vendored build (used via `openssl = { features = ["vendored"] }`) |
| `elfutils-libelf-devel` | libbpf-sys crate (provides `gelf.h`, `libelf.h`) |
| `perl-IPC-Cmd` | OpenSSL source build (Perl IPC::Cmd module) |
| `libbpf-devel` | eBPF program compilation and loading |
| `clang` / `llvm` | eBPF C program compilation to BPF bytecode |
| `bpftool` | eBPF skeleton generation |

You can verify all dependencies with the included check script:
```bash
./scripts/check-deps.sh
```

#### Version Requirements

| Component | Version |
|-----------|---------|
| Linux kernel | >= 5.8 (BTF support) |
| Rust | >= 1.80 |
| clang / llvm | >= 11 (for eBPF compilation) |
| libbpf | >= 0.8 |

### Install with Anolisa

```bash
sudo anolisa --install-mode system install agentsight
```

AgentSight requires Linux system mode. This installs the AgentSight service and
the `agentsight-enforcer` service together.

### Install via RPM

```bash
sudo yum install agentsight
```

Installs:
- `/usr/local/bin/agentsight` — CLI binary
- `/usr/local/bin/agentsight-enforcer` — ActPlane enforcement engine
- `/usr/lib/systemd/system/agentsight.service` — AgentSight system unit
- `/usr/lib/systemd/system/agentsight-enforcer.service` — enforcement system unit

The RPM is a Linux system package. Its units are installed but not enabled by
default; when both units run, AgentSight is ordered after the enforcer.

### Start the Service

Both package routes leave the units stopped and disabled. Start the main unit
when you are ready to begin collection:

```bash
sudo systemctl enable --now agentsight.service
sudo systemctl status agentsight.service
```

The main unit runs eBPF tracing and the Dashboard together and starts the
enforcer dependency in the required order. Open `http://localhost:7396` after
the service becomes active.

The unit runs as root with `UMask=0077`, so its data under
`/var/log/sysak/.agentsight` is private. Use `sudo` for CLI queries and
Dashboard access commands that read service-owned data. Stop the unit before
starting a foreground tracer.

### Kubernetes DaemonSet

For node-wide collection in Kubernetes, use the DaemonSet manifest and runtime
image under `src/agentsight/packaging/` (`k8s/daemonset.yaml` and
`docker/Dockerfile`). See the
[deployment guide](../../docs/user-guide/en/agentsight/deployment.md#kubernetes-daemonset-node-wide)
for prerequisites and verification.

### Build from Source

```bash
cd src/agentsight

# Verify dependencies (optional but recommended)
./scripts/check-deps.sh

# Build frontend and Rust binary with embedded Dashboard UI
make build-all
```

The binary is output to `target/release/agentsight`. On supported Linux systems,
`make build-all` also invokes `scripts/build-enforcer.sh` to build the attested
ActPlane enforcer in a new private directory. The script prints its absolute
path; keep the adjacent `.sha256` receipt and pass that exact path as
`ENFORCER_BIN` to the raw packager. A shared `target/release/agentsight-enforcer`
is not a verified build result. `make build-mac` does not build the enforcer.

For Linux cross builds, `scripts/build-enforcer.sh --target
<aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu>` selects the target explicitly;
without arguments it builds for the host. `CARGO` may select a prepared Cross
wrapper, but must honor the requested target. Both Linux architectures use the
same raw payload layout and source-owned `.anolisa/component.toml`. macOS ARM64
uses `.anolisa/component.macos.toml` and ships only the main executable. Cross
compiling macOS from Linux requires `--no-default-features --features server` to
skip the optional `bpf-build` host dependencies. Remove
the private enforcer directory only after packaging has consumed its receipt.

> `cargo build --release` only compiles Rust. It does not rebuild the embedded Dashboard UI, so use `make build-all` for user-facing builds.

### GitHub prebuilt artifacts

Release Preview builds Linux x86_64, Linux ARM64 and macOS ARM64 from the selected
source commit; formal releases additionally require `sight/v<version>` to point
at that commit. Each target produces `agentsight-<version>-<os>-<arch>.tar.gz`,
its `.sha256`, a CycloneDX `.cdx.json` SBOM and the SBOM's `.sha256` (12 files in
all). Linux targets require glibc 2.28; macOS ARM64 retains the 11.0 baseline.
The action builds the embedded viewer and invokes this component's packager and
contract from the same revision. Linux SBOMs include the attested enforcer's
actual dependency graph. Download verification covers all three targets before
Release publication. It does not attest ARM64 real-kernel eBPF behavior.

### Build on macOS

macOS builds `agentsight trace` (trajectory collector) and `agentsight serve` (Dashboard viewer). It does not require libbpf, clang/llvm, kernel headers, root, or Linux BPF capabilities.

**Prerequisites:**

| Component | Version | Required for |
|-----------|---------|-------------|
| Rust | >= 1.80 | Compile Rust code |
| Node.js | >= 16 | Frontend build |
| npm | >= 8 | Frontend dependency management |

**Build steps:**

```bash
cd src/agentsight

# Build the frontend and the macOS binary
make build-mac
```

The binary is output to `target/release/agentsight`.

**Usage on macOS:**

```bash
# Terminal 1: collect trajectories (scans JSONL → trajectories.db)
agentsight trace

# Terminal 2: start the Dashboard + trajectory viewer
agentsight serve

# Or bind to a custom host/port
agentsight serve --host 0.0.0.0 --port 8080
```

Open `http://127.0.0.1:7396` to view the Agent Dashboard. `trace` scans local AI agent session files (Claude Code, Qoder, Codex, Cursor) and stores them as ATIF trajectories in `trajectories.db`. `serve` reads from the same database.

> **macOS limitations**: eBPF-dependent commands (`discover`, `token`, `audit`, `metrics`, `interruption`, `skill-metrics`, `summary`) are Linux-only. The `--db` and `--config` flags are also Linux-only. On macOS, `trace` collects trajectories only (no eBPF), and `serve` reads from `trajectories.db`.

### Start Tracing

```bash
# Requires root for eBPF
sudo agentsight trace
```

## Configuration

AgentSight is configured via `agentsight.json` (default path `/etc/agentsight/config.json`; falls back to embedded defaults if absent).

### SQLite Storage (`storage`)

`agentsight.json` schema v5 gives every AgentSight-owned SQLite store the same three policy keys:
`retention_days`, `max_db_size_mb`, and `check_interval_secs`. A zero value disables the
corresponding age, capacity, or scheduled-maintenance rule. Primary and GenAI maintenance default
to 60 seconds; reuse, causal, and enforcement stores also have explicit policies. One lightweight
maintenance thread per long-running process schedules all of that process's databases, while
per-database locks coordinate `trace` and `serve`. Cleanup removes expired records first, checkpoints
before size pruning, and targets 90% logical usage after physical allocation crosses the limit;
automatic maintenance never runs `VACUUM` and leaves freed pages reusable. The Dashboard Settings
page shows policy, capacity, coverage, and worker health without exposing database paths. See the
[configuration guide](../../docs/user-guide/en/agentsight/configuration.md#sqlite-storage-policies).

### Feature Flags (`features`)

Feature defaults are listed below. Disable optional features via the `features` block in `agentsight.json` to reduce memory and I/O overhead:

| Feature | JSON Path | Default | Description |
|---------|-----------|---------|-------------|
| Token Stats | `features.token_stats` | `true` | Core functionality, not recommended to disable |
| Local Tokenizer | `features.tokenizer.enabled` | `false` | HuggingFace model fallback (50–100 MB per model) |
| Session Mapping | `features.session_mapping.enabled` | `true` | responseId → sessionId correlation (LRU 10,000) |
| SQLite Storage | `features.sqlite_storage.enabled` | `true` | Persist to disk SQLite; disabled uses noop store |
| Resource Sampling | `features.resource_sampling` | `false` | Sample Agent CPU/RSS once per second; requires SQLite storage |
| Interruption Detection | `features.interruption_detection.enabled` | `true` | Dead loop / crash / context overflow detection |
| Audit | `features.audit` | `true` | LLM call audit event persistence |
| Token Consumption | `features.token_consumption` | `false` | Aggregated token consumption records |
| SLS Logtail | `features.sls_logtail` | `false` | Write to SLS log file |
| Trajectory Collection | `features.trajectory_collection.enabled` | `false` | Periodically scan Qoder/QoderWork session JSONL, convert to ATIF v1.7 and store in `trajectories.db` (trace mode only; `scan_interval_secs` default 30, `scan_dirs` overrides scan roots) |

### Runtime Resource Limits (`runtime_limits`)

Configure buffer caps to prevent unbounded memory growth:

| Option | Default | Description |
|--------|---------|-------------|
| `event_channel_capacity` | 10,000 | Bounded channel capacity for probe events |
| `event_channel_policy` | `"backpressure"` | Wait for a queue slot after byte admission; `drop_newest` and `sample:N` remain available |
| `event_channel_max_bytes_mb` | 64 | Byte budget for queued probe events (one SSL record reaches 4 MiB, so the slot count alone cannot bound memory) |
| `pending_genai_max_count` | 1,000 | Max pending events awaiting session_id |
| `pending_genai_max_bytes_mb` | 16 | Max bytes for pending events |
| `pid_cache_size` | 1,024 | PID → agent_name LRU cache size |
| `connection_capacity` | 128 | HTTP connection cache entries; use at least the expected concurrency and restart tracing |
| `max_connection_body_mb` | 8 | Per-connection HTTP body buffer cap |
| `connection_idle_timeout_secs` | 60 | Soft inactivity timeout for pending SQLite snapshots |
| `connection_hard_timeout_secs` | 300 | Finalize inactive calls as incomplete and release them; 0 disables the timeout |
| `connection_max_bytes_mb` | 64 | Shared HTTP/1 + HTTP/2 retained payload budget |
| `ffi_channel_max_bytes_mb` | 64 | FFI payload budget, including active callbacks |
| `max_sse_retained_mb` | 2 | Per-stream SSE payload limit |
| `retain_raw_sse` | `false` | Preserve original SSE sequences for raw export or debugging |
| `ring_buffer_mb` | 64 | eBPF Ring Buffer size (must be power of 2) |

Semantic-only delivery reduces SSE deltas and releases raw TLS buffers during the stream.
In-flight connections active within the last 5 seconds are protected from capacity eviction,
including request assembly and response waiting; when all
entries are protected, new connections are rejected with `abandon_reason=capacity_rejected`.
Completed HTTP/1 connections reserve their slot for 5 seconds using only an identifier and
timestamp, allowing the next keep-alive request to reuse it without retaining payloads.
Resource removals report bounded metadata with `incomplete=true` and `metadata_only=true`.
Hard byte limits still permit forced reclamation.
Probe and FFI queues enforce byte limits alongside their slot limits and count rejected events.
These budgets cover retained payloads; process RSS also includes the ring buffer, decoder working
memory and allocator overhead. See the [configuration reference](../../docs/user-guide/en/agentsight/configuration.md#runtime-limits)
for timeout, raw capture and overload behavior.

Configuration schema v5 ships these defaults. Startup upgrades back up and replace older config
files; reapply custom rules and settings from the backup. The implementation is described in
[HTTP payload ownership and memory limits](docs/design/payload-memory.md).

### Runtime Metrics Export

Set `AGENTSIGHT_METRICS_FILE` to write Prometheus runtime metrics to a file.
`AGENTSIGHT_METRICS_INTERVAL_SECS` sets the minimum time between updates in
positive whole seconds (default: `1`). A final snapshot is written on shutdown
regardless of this interval. The interval setting has no effect when the metrics
file is not enabled.

### Minimal Memory Configuration

For resource-constrained environments, disable non-essential features and reduce ring buffer:

```json
{
  "features": {
    "token_stats": true,
    "tokenizer": { "enabled": false },
    "session_mapping": { "enabled": false },
    "sqlite_storage": { "enabled": false },
    "interruption_detection": { "enabled": false },
    "audit": false,
    "token_consumption": false,
    "sls_logtail": false
  },
  "runtime_limits": {
    "ring_buffer_mb": 8,
    "event_channel_capacity": 5000,
    "pending_genai_max_count": 500,
    "pending_genai_max_bytes_mb": 32
  }
}
```

> With this config: idle RSS ~24–30 MB, with event traffic ~35–40 MB.

## ATIF export

Use `scripts/batch-analyze.py --export-atif DIRECTORY` to save converted Claude/Qoder ATIF locally without invoking an analyzer.
See the [CLI reference](../../docs/user-guide/en/agentsight/cli-reference.md) for usage and limits.

## Supported LLM Providers

Token parsing supports multiple LLM API formats:

- OpenAI / OpenAI-compatible APIs
- Anthropic (Claude, including cache token handling)
- Google Gemini
- Qwen (with native chat template support)

## Batch concurrency

The batch script accepts opt-in `--jobs 1..8` (default 1), with bounded concurrent invocations and ordered output.
See the [CLI reference](../../docs/user-guide/en/agentsight/cli-reference.md) for usage and limits.

## Origins

This project is derived from [https://github.com/eunomia-bpf/agentsight.git](https://github.com/eunomia-bpf/agentsight.git).

## License

Apache License 2.0 — see [LICENSE](../../LICENSE) for details.

Remote Tokenless statistics can be recorded through the optional `tokenless_api` configuration; see [configuration](../../docs/user-guide/en/agentsight/configuration.md#tokenless-statistics-api).

# AgentSight CLI Reference

[中文版](../../zh/agentsight/cli-reference.md)

All flags on this page are taken from `agentsight 0.13.0 --help` on Linux. The sample output keeps
the real layout, but every identifier is a placeholder and every count is a round number — none of
it is a real capture.

## Conventions

| Point | Detail |
|---|---|
| Privileges | `trace` needs root (or `CAP_BPF` + `CAP_PERFMON`). Query commands need read access to `/var/log/sysak/.agentsight`, which the packaged service owns — use `sudo`. |
| Config file | Commands that read rules accept `--config`, default `/etc/agentsight/config.json`. `discover` reads the same file `trace` does, so it reflects your custom rules. |
| Data location | Fixed at `/var/log/sysak/.agentsight`. `serve`, `dashboard`, `skill-metrics`, `audit`, `metrics`, `summary`, `interruption`, and `token` accept `--db` to point at a different database file. |
| Machine output | `--json` is available on `discover`, `token`, `metrics`, `audit`, `summary`, `interruption *`, and `skill-metrics *`. |
| Output language | `summary`, `metrics`, and `interruption` print English; `discover` and `token` print Chinese regardless of locale. Use `--json` for stable, language-neutral text. |
| Platform | On macOS only `trace` (trajectory collector) and `serve` exist. |
| Sample IDs | Session, conversation, trace, and interruption IDs in the examples are placeholders — substitute the ones from your own output. |

```
$ agentsight --help
agentsight 0.11.x
AI Agent observability tool - trace processes, SSL traffic, and LLM API calls via eBPF

SUBCOMMANDS:
    audit            Query audit events
    dashboard        Display dashboard URL and ECS console access guide
    discover         Discover running AI agents on the system
    interruption     Query and manage session interruption events detected during agent conversations
    metrics          Print per-agent token usage metrics in Prometheus text format
    serve            Start the API server
    skill-metrics    Compute and display skill usage metrics
    summary          Print a unified summary of sessions, interruptions, and tokenless savings
    token            Query token consumption data
    trace            Trace agent activity (default)
```

## Resource benchmark helper (Linux)

The repository's `resource_run.sh --ffi` measures AgentSight through a minimal
C FFI host, reusing the normal load and CPU/RSS reports. `--ffi-api typed|v2`
selects callbacks; `--candidate-lib` and `--baseline-lib` accept prebuilt libraries.
FFI readiness and capture validation read callback receipts without requiring SQLite storage.
Tracing still requires root or eBPF capabilities. See the
[benchmark usage guide](../../../../src/agentsight/scripts/docs/BENCHMARK_USAGE.md#71-measure-the-ffi-library-with-a-minimal-c-host)
for build requirements, A/B options and measurement boundaries.

## Offline trajectory statistics

From a source checkout, run these commands in `src/agentsight`:

```bash
cargo run -q -p agentsight-opt --bin analyze -- trajectory.json --dim perf --stats-only
cargo run -q -p agentsight-opt --bin analyze -- trajectory.json --dim cost --stats-only
python3 scripts/batch-analyze.py --file session.jsonl --dim cost --stats-only --min-steps 1
```

`trajectory.json` is an ATIF document; `session.jsonl` is a Claude/Qoder session.
`--stats-only` calls the existing local computations without an API key, model request,
or eBPF privileges. Output contains `perf` or `cost` statistics; it omits the LLM-only
`perf_issues` and `cost_waste` judgments. Cost statistics describe recorded tokens and
estimated replay headroom, not a provider billing invoice. `accuracy --stats-only`
is rejected because accuracy attribution needs model judgment.

Without the flag, the existing full analysis still requires `OPENAI_API_KEY` and can
make paid model requests. The batch wrapper forwards the flag to its analyzer; a
custom `--binary` must support it. These are source-checkout tools, separate from
the installed `agentsight` subcommands.

## agentsight trace

Loads the eBPF probes, discovers Agent processes, and writes captured events to SQLite.

```
FLAGS:
        --daemon              Run as daemon in background (Linux only)
        --enable-filewatch    Enable file watch probe (monitors .jsonl file opens from traced processes)
        --no-ebpf             Skip eBPF probes and collect trajectories only (Linux only)
    -v, --verbose             Enable verbose/debug output

OPTIONS:
    -c, --config <config>        Path to JSON configuration file (Linux only) [default: /etc/agentsight/config.json]
        --pid-file <pid-file>    PID file path for daemon mode (Linux only) [default: /tmp/agentsight.pid]
```

```bash
# foreground (stop the packaged service first, only one tracer should run)
sudo systemctl stop agentsight.service
sudo agentsight trace

# background with a custom rule file
sudo agentsight trace --daemon -c /etc/agentsight/config.json

# unprivileged host: trajectory collection only, no root and no CAP_BPF needed
agentsight trace --no-ebpf
```

> Running two tracers at the same time makes both compete for the same uprobes and produces
> confusing data. Stop `agentsight.service` before starting a foreground tracer.

### Running without privileges (`--no-ebpf`)

Loading the probes requires root or `CAP_BPF`/`CAP_PERFMON`. Without them `trace` stops at probe
setup and reports `Failed to create probes`. `--no-ebpf` skips the probes and runs only the
trajectory collector, which is pure user-space: it scans local JSONL sessions (Claude Code, Qoder,
QoderWork, Codex) and Hermes `state.db`, converts them to ATIF v1.7, and stores them in
`trajectories.db` for `serve` to display.

What this mode does **not** provide: token metering, audit events, interruption detection, and
process or TLS traffic monitoring all come from the probes, so `agentsight token`, `audit`,
`metrics`, and `interruption` report no new data.

Two behaviours to keep in mind:

- The flag implies trajectory collection even when `features.trajectory_collection.enabled` is
  `false`, because it is the only remaining data source. `scan_interval_secs` and `scan_dirs` are
  still read from the configuration file.
- `trajectories.db` goes to the shared data directory when it is writable, otherwise to
  `$HOME/.local/share/agentsight/`. Startup prints the resolved path together with the matching
  `serve --db` command; pass that path to `serve` when the fallback is used. The fallback directory
  and database are created owner-only (`0700`/`0600`), because trajectories embed full conversation
  content.

JSONL discovery scans `/root` and `/home/*` by default and also honors an inherited
`CLAUDE_CONFIG_DIR`. Hermes discovery checks `$HERMES_HOME/state.db` and `~/.hermes/state.db`.
If `scan_dirs` is set, it overrides these defaults. A configured path may be a session root, a
Hermes home/database, or a workspace containing per-session `.claude/projects` trees. Nested
Claude discovery checks up to four directory levels with a 4096-directory budget and does not
follow directory symlinks. When SDK options set a non-standard subprocess-only config directory,
expose the same `CLAUDE_CONFIG_DIR` to the AgentSight process and leave `scan_dirs` unset.

## agentsight serve

Serves the HTTP API and the embedded Dashboard from the same databases the tracer writes.

```
OPTIONS:
        --config <config>    Path to JSON configuration file (Linux only) [default: /etc/agentsight/config.json]
        --db <db>            Custom database path (Linux only)
        --host <host>        Host to bind to [default: 127.0.0.1]
        --port <port>        Port to bind to [default: 7396]
```

```bash
# local only
sudo agentsight serve

# reachable from other hosts (restrict the port in your firewall first)
sudo agentsight serve --host 0.0.0.0 --port 7396

# browse an archived database without tracing
agentsight serve --db /backup/genai_events.db
```

Run `serve` as the same user as `trace`, otherwise the two resolve different data directories.

## agentsight dashboard

Prints the Dashboard URLs and the access token, then tries to open a browser. On ECS instances it
also prints the security-group configuration link.

```
FLAGS:
        --no-open          Do not attempt to open a browser
        --skip-sg-guide    Skip ECS security group guide output

OPTIONS:
        --config <config>    Path to JSON configuration file [default: /etc/agentsight/config.json]
        --db <db>            Custom database path (used to locate the token file)
        --host <host>        Host the server is bound to (use a specific IP/hostname to override the Network URL) [default: 0.0.0.0]
        --port <port>        Port the server is listening on [default: 7396]
```

```bash
$ sudo agentsight dashboard --no-open

AgentSight 仪表盘状态
=====================

  认证:    已启用
  本机:    http://127.0.0.1:7396 (无需认证)
  局域网:   http://192.168.1.10:7396/?token=<TOKEN>
  公网:    http://203.0.113.10:7396/?token=<TOKEN>
```

Use `--no-open` on servers; opening a browser as root is rarely what you want.

## agentsight summary

One-command health picture for a recent window.

```
FLAGS:
        --json                          Output as JSON
OPTIONS:
        --agent <agent>                 Scope the session and interruption totals to one agent
        --db <db>                       Override the sessions database path
        --last <last>                   Query the last N hours (default: 24) [default: 24]
        --tokenless-db <tokenless-db>   Override the tokenless stats database path
```

```bash
$ sudo agentsight summary --last 24
AgentSight Summary (last 24h)

Sessions      10
  Tokens      100.0K in / 10.0K out / 110.0K total

Interruptions 1
  critical    0
  high        0
  medium      1
  low         0

Tokenless     10% saved (110.0K -> 99.0K, 20 ops)
```

`--db` reads a copy of the databases — for example one copied off another host — instead of the
default directory, so the report needs no `sudo` and mixes in no live data. The interruption
database is read from the same directory (`interruption_events.db`), matching the default layout.
The tokenless store lives outside those databases, in the caller's home directory, so `--db` does
not move it: pass `--tokenless-db` to read a copy of `stats.db` and keep the whole report free of
the reading host's own data.

```bash
agentsight summary --last 24 --db /backup/agentsight/genai_events.db
```

`--agent` narrows the Sessions and Interruptions sections to one agent, and the two sections match
the same way their own stores do. Sessions compare the label the session listing already resolves —
the recorded agent name, falling back to the process name — while interrupts compare the
`agent_name` column only, which is what `interruption list --agent` filters on. Both matches are
case-insensitive. The heading and the JSON gain an `agent` key naming the scope; without the flag
both sections cover every agent and the report keeps its previous shape.

```bash
agentsight summary --last 24 --agent CoshNG
```

Tokenless savings are recorded per session and carry no agent, so an agent-scoped report leaves
that section out and says so rather than printing host-wide numbers beside agent-scoped totals.

## agentsight token

Token consumption for a period, optionally compared with the previous one.

```
FLAGS:
        --compare    Compare with previous period
        --json       Output as JSON

OPTIONS:
        --data-file <data-file>    Custom data file path
        --db <db>                  Override the database path (same as --data-file)
        --hours <hours>            Query last N hours
        --period <period>          Query by fixed time period
                                   [possible values: today, yesterday, week, last_week, month, last_month]
```

`--db` is the same override every other query command spells that way; it was added after
`--data-file`, which stays supported. Pass one or the other — giving both is a usage error.

```bash
# read a copy of the database instead of the default location
agentsight token --db /backup/agentsight/agentsight.db --period week
```

```bash
$ sudo agentsight token --json
{
  "period": "今天",
  "input_tokens": 100000,
  "output_tokens": 10000,
  "total_tokens": 110000,
  "request_count": 20,
  "comparison": null,
  "breakdown": []
}

# week over week
sudo agentsight token --period week --compare
```

## agentsight audit

Queries the audit trail: LLM calls and process actions.

```
FLAGS:
        --json       Output as JSON
        --summary    Show summary statistics

OPTIONS:
        --type <event-type>       Filter by event type: "llm" or "process"
        --exclude <exclude>...    Hide process_action events whose command/args contain any of these
                                  substrings. Repeatable. The hidden count is reported
        --last <last>             Query last N hours (e.g. 24)
        --pid <pid>               Filter by PID
        --db <db>                 Override the database path
```

```bash
$ sudo agentsight audit --summary
=== Audit Summary (last 24 hours) ===

LLM calls:        20
Process actions:  100

Providers:
  openai: 20 calls

Top commands:
  agent-sec-cli scan-pii --stdin --format json --redact-output --source observability ...: 40 times
  sh -c python3 /usr/share/anolisa/extensions/agent-sec-core/hooks/observability_hook.py ...: 30 times
  ...
```

Individual events are JSON, so `jq` works directly. Use `--json`, which emits one array, and
iterate over it — the plain form prints a human-readable header line before the records, which `jq`
cannot parse:

```bash
sudo agentsight audit --last 24 --type llm --json \
  | jq -r '.[] | [.extra.model, .extra.input_tokens, .extra.output_tokens] | @tsv'
```

Hook and wrapper commands dominate `process_action` on hosts that run agent-sec-core or Tokenless.
Filter them out with repeated `--exclude`:

```bash
sudo agentsight audit --last 1 --exclude agent-sec-cli --exclude observability_hook.py
```

`--db` reads a copy of `agentsight.db` — for example one copied off another host — instead of the
default directory:

```bash
agentsight audit --last 24 --db /backup/agentsight/agentsight.db
```

## agentsight discover

Shows which Agent processes are running and which rules exist.

```
FLAGS:
        --list-known    List all known agents and show currently matched PIDs
    -v, --verbose       Show detailed output including executable path

OPTIONS:
    -c, --config <config>    Path to JSON configuration file [default: /etc/agentsight/config.json]
```

```bash
$ sudo agentsight discover
已发现 AI Agent（共 1 个）:
============================================================

  CoshNG [PID: 10000]
    类别: custom
    命令:  /usr/libexec/anolisa/cosh-ng/cosh-shell ...

总计: 1 个 Agent

$ sudo agentsight discover --list-known | head -12
已知 AI Agent（共 31 条规则）:
============================================================

  Hermes (custom)
    命令行规则: hermes*
    运行中 PID: 无
    Config-driven agent
```

`discover` and `discover --list-known` read the same config file `trace` uses (`--config`, default
`/etc/agentsight/config.json`), so `--list-known` reflects the rules actually in effect — including
ones you added. If the file is missing or unparseable, it prints a hint and falls back to the
built-in rules. See [Agent discovery rules](configuration.md#agent-discovery-rules).

## agentsight metrics

Prometheus text output for per-agent Token counters (all-time).

```bash
$ sudo agentsight metrics | head -8
# HELP agentsight_token_input_total Total input tokens consumed by agent (all-time)
# TYPE agentsight_token_input_total counter
agentsight_token_input_total{agent="CoshNG"} 100000
agentsight_token_input_total{agent="Cosh"} 50000

# HELP agentsight_token_output_total Total output tokens consumed by agent (all-time)
# TYPE agentsight_token_output_total counter
agentsight_token_output_total{agent="CoshNG"} 10000
```

`--json` prints the same per-agent rows as objects (`agent_name`, `input_tokens`, `output_tokens`,
`total_tokens`, `request_count`) instead of the exposition format, for scripts that would otherwise
have to parse it:

```bash
$ sudo agentsight metrics --json
[
  {
    "agent_name": "CoshNG",
    "input_tokens": 100000,
    "output_tokens": 10000,
    "total_tokens": 110000,
    "request_count": 42
  }
]
```

The running server exposes the same content at `GET /metrics` (localhost only), which is usually
easier to scrape. See [Data and storage](data-and-storage.md#prometheus-metrics).

`--db` reads a copy of `genai_events.db` instead of the default directory:

```bash
agentsight metrics --db /backup/agentsight/genai_events.db
```

## agentsight interruption

Queries and resolves detected interruptions. Database:
`/var/log/sysak/.agentsight/interruption_events.db`. `--db` (before the subcommand) reads a copy of
the database instead — for example one copied off another host:

```bash
agentsight interruption --db /backup/agentsight/interruption_events.db stats --last 168
```

```
SUBCOMMANDS:
    list            List interruption events with optional filters
    get             Get a single interruption event by its ID
    stats           Show per-type count statistics within a time range
    count           Count unresolved interruptions grouped by severity
    session         List all interruption events for a specific session
    conversation    List all interruption events for a specific conversation
    resolve         Mark an interruption event as resolved
```

`list` options:

```
FLAGS:
        --json          Output as JSON (one JSON array)
        --resolved      Show only resolved events
        --unresolved    Show only unresolved events

OPTIONS:
        --agent <agent>          Filter by agent name (exact match)
        --type <itype>           [possible values: agent_crash, rate_limit, auth_error,
                                                   network_timeout, service_unavailable,
                                                   safety_filter, sse_truncated, context_overflow,
                                                   token_limit, llm_error, retry_storm, dead_loop,
                                                   tool_failure, empty_response, resource_exhaustion,
                                                   slow_response, state_machine_error,
                                                   unauthorized_action]
        --last <last>            Query last N hours (default: 24) [default: 24]
        --limit <limit>          Maximum number of results (default: 100) [default: 100]
        --severity <severity>    [possible values: critical, high, medium, low]
```

`stats` and `count` take the same `--agent` filter `list` does, so one agent's breakdown can be
read without post-processing `list --json`:

```
FLAGS:
        --json          Output as JSON

OPTIONS:
        --agent <agent>          Filter by agent name (exact match)
        --last <last>            Query last N hours (default: 24) [default: 24]
```

`stats` additionally takes the `--unresolved` / `--resolved` pair, so the per-type table can
answer "what is still open in this window" without also counting events someone already closed:

```
FLAGS:
        --json          Output as JSON
        --resolved      Count only resolved events
        --unresolved    Count only unresolved events
```

```
$ sudo agentsight interruption stats --last 168 --unresolved
TYPE                 SEVERITY    COUNT
----------------------------------------
token_limit          medium          3
llm_error            high            1
```

Without either flag `stats` keeps its previous scope and reports the whole window (resolved and
unresolved); the two flags are mutually exclusive. `count` is unchanged and still reports
unresolved events only. Without `--agent` both cover every agent on the host.

`count` also takes `list`'s `--type`, narrowing the per-severity totals to one interruption type
with the same closed value set, so "how many `token_limit` hits are still open" is one call instead
of a `list --type` array summed by hand:

```
$ sudo agentsight interruption count --last 24 --type token_limit --json
{
  "total": 2,
  "by_severity": {
    "critical": 0,
    "high": 0,
    "medium": 2,
    "low": 0
  }
}
```

Without `--type` the totals cover every type, as before, and the human-readable form keeps its
layout. A type that matched nothing reports zeros, not the unfiltered totals.

```bash
$ sudo agentsight interruption list --last 24
INTERRUPTION_ID                    TYPE          SEVERITY  OCCURRED_AT              RESOLVED  AGENT    SESSION_ID
------------------------------------------------------------------------------------------------------------------
11111111222222223333333344444444   token_limit   medium    2026-01-01 12:00:00.000  no        CoshNG   00000000-11...

Total: 1 event(s)

$ sudo agentsight interruption get 11111111222222223333333344444444
Interruption Event Detail
============================================================
  ID:           11111111222222223333333344444444
  Type:         token_limit
  Severity:     medium
  Occurred At:  2026-01-01 12:00:00.000 (1767268800000000000ns)
  Resolved:     no
  Session ID:   00000000-1111-2222-3333-444444444444
  Conversation: aaaaaaaabbbbbbbbccccccccdddddddd
  Trace ID:     chatcmpl-00000000-1111-2222-3333-444444444444
  PID:          10000
  Agent:        CoshNG
  Detail:
{
  "model": "qwen-plus",
  "output_tokens": 4096,
  "max_tokens": 4096,
  "ratio": 1.0
}
```

```bash
# per-type counts for the last week
sudo agentsight interruption stats --last 168

# the same breakdown for one agent only
sudo agentsight interruption stats --last 168 --agent CoshNG

# unresolved counts by severity
sudo agentsight interruption count --last 24

# unresolved counts for one agent only
sudo agentsight interruption count --last 24 --agent CoshNG

# everything that hit one session, then close one event
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444
sudo agentsight interruption resolve 11111111222222223333333344444444

# the same close, with a machine-readable answer
sudo agentsight interruption resolve 11111111222222223333333344444444 --json

# only what is still open in that session
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444 --unresolved

# only the token-limit hits in that session
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444 --type token_limit
```

`resolve` also accepts `--json`, so the one `interruption` subcommand that writes the database
answers in the same machine-readable vocabulary as the read commands. Without the flag it keeps the
single line it has always printed:

```
$ sudo agentsight interruption resolve 11111111222222223333333344444444 --json
{
  "status": "resolved",
  "interruption_id": "11111111222222223333333344444444"
}
```

`status` is the field the HTTP route `POST /api/interruptions/{id}/resolve` answers with; the CLI
adds `interruption_id`, which the route only carries in its path. An unknown id still exits non-zero with the message on
stderr in both forms: `--json` changes the success body, not the failure path.

`session` and `conversation` take the same `--unresolved` / `--resolved` pair `list` and `stats`
do, so a per-id view can be narrowed to the events still open (or already closed) without reading
the `RESOLVED` column by eye. They also accept `list`'s `--type` and `--severity` filters, with the
same closed value sets, so one session can be read down to its critical `agent_crash` rows without
post-processing the `--json` array:

```
FLAGS:
        --json          Output as JSON (one JSON array)
        --resolved      Show only resolved events
        --unresolved    Show only unresolved events

OPTIONS:
        --severity <severity>    [possible values: critical, high, medium, low]
        --type <itype>           [possible values: agent_crash, rate_limit, auth_error,
                                                   network_timeout, service_unavailable,
                                                   safety_filter, sse_truncated, context_overflow,
                                                   token_limit, llm_error, retry_storm, dead_loop,
                                                   tool_failure, empty_response, resource_exhaustion,
                                                   slow_response, state_machine_error,
                                                   unauthorized_action]
```

The resolution pair stays mutually exclusive, and every filter defaults to off: without flags the
command prints the id's whole history, as before. `--agent` is deliberately not part of these two
views — the resolver derives a session from `(agent, pid, prompt)`, so one session already belongs
to one agent. Scope by agent on `list`, `stats`, or `count` instead.

> `--type` accepts every interruption type the detector can produce (the values are derived from
> `InterruptionType::ALL`). `agentsight interruption list --help` prints the current list; the full
> catalog with triggers is in [Interruption detection](interruption-detection.md#interruption-types).

## agentsight skill-metrics

Computes Skill usage metrics on demand by scanning GenAI events.

```
SUBCOMMANDS:
    all             Compute all skill metrics
    downloads       Show skill download tracking (first appearance in available_skills)
    loads           Show skill load counts (SKILL.md reads via tool_calls)
    usage-ratio     Show skill usage ratio (tasks with/without skills)
    distribution    Show per-task skill count distribution
    hotness         Show skill hotness ranking by week

OPTIONS (all subcommands):
        --agent <agent>    Filter by agent name
        --db <db>          Override database path
        --last <last>      Query last N hours (default: 168 = 7 days) [default: 168]
        --json             Output as JSON

OPTIONS (all, hotness):
        --granularity <granularity>    Bucket the hotness trend by day or week
                                       [possible values: day, week] [default: week]
```

```bash
sudo agentsight skill-metrics all --last 168
sudo agentsight skill-metrics hotness --agent CoshNG --json

# daily buckets instead of the weekly default
sudo agentsight skill-metrics hotness --last 168 --granularity day
```

The same numbers appear on the Dashboard's Skill Metrics page, which switches the same
day/week granularity on its hotness chart. `--granularity` buckets only the hotness trend, so
`all` and `hotness` accept it and the four metric-specific subcommands do not — passing it
there is a usage error, the same refusal `GET /api/skill-metrics/downloads` and its siblings
return. Without the flag the trend keeps its weekly buckets, and the text and JSON output are
unchanged.

## Export batch ATIF locally

From `src/agentsight` in a source checkout:

```bash
python3 scripts/batch-analyze.py --file session.jsonl --export-atif ./atif-export --min-steps 1
```

`--export-atif DIRECTORY` writes the converted document without launching an analyzer
or making model requests. It is mutually exclusive with `--dim`, and rejects `--binary`.
Selection is shared with analysis: `--file`, repeatable `--dir`, `--limit` (default 10),
and `--min-steps` (default 3). Directory scans keep the existing 1 KiB minimum file size
and subagent exclusion; use `--file` to export a small file explicitly.

Each name is the full SHA-256 of the canonical absolute source path followed by
`.atif.json`. Same-name inputs in different directories remain distinct; exporting the
same path again atomically replaces its previous artifact. Moving the source changes
its artifact name. Existing unrelated destination files and source files are preserved.
The directory is created on demand. Stdout emits one JSON record per exported file with
`_atif_file`, `_source_file`, `_session_id`, and `_steps`.

Exports use UTF-8 and the existing converter's filtering and truncation rules; they are
not a lossless backup of raw logs. They contain conversation content, so keep them in a
local directory intended for that data. Write failures are counted in stderr's summary,
temporary files are removed, and later files are attempted. As in existing batch analysis,
a completed batch returns 0 even if its summary includes per-file failures.

## Related pages

- [Configuration](configuration.md) — what the config file controls
- [Dashboard guide](dashboard.md) — UI equivalents of these queries
- [Data and storage](data-and-storage.md) — HTTP API and database layout

## Parallel batch analysis

The source-checkout batch tool can run independent analyzer invocations concurrently.
From `src/agentsight`, build the analyzer once and opt in to up to four workers:

```bash
cargo build -p agentsight-opt --bin analyze
python3 scripts/batch-analyze.py --dir /path/to/sessions --dim perf --binary target/debug/analyze --jobs 4
```

`--jobs N` accepts 1 through 8 and defaults to 1. Both running invocations and outstanding
work are bounded by N. Documents and per-file diagnostics are emitted in the original
discovery order, even when later invocations finish first; this may delay visible output
behind a slow earlier file. Each document retains its source/session metadata.

Existing selection, minimum-step filtering, timeout, failure counters, exit behavior and
temporary-file cleanup remain in effect. Prebuilding avoids concurrent Cargo build-lock
contention. Full analysis still requires model credentials; choosing N above 1 may issue
simultaneous paid requests and encounter endpoint rate limits. This reduces elapsed time
for independent sessions where concurrency is available; it does not reduce per-request
model latency, token use or prices.

## Benchmark artifacts

From the repository root, `bash src/agentsight/scripts/benchmark/single_run/resource_run.sh --help`
lists controls for the Linux standalone resource benchmark (formerly `memory_run.sh`).
It defaults to 32 Agents at 100 output tok/s each for 60 seconds and requires root
or eBPF capabilities. `resource-report.txt` and `resource-summary.json` report
load-window average/peak RSS and average/P95/peak CPU, with response drain separate.
CPU 100% means one logical CPU. Cooperative pacing defaults to 25%.
`--cpu-budget-percent N` overrides the target; `0` disables it, and omission uses the configured
target or 25% when unset. Check response drain and queue growth
alongside CPU and RSS. See the
[benchmark usage guide](../../../../src/agentsight/scripts/docs/BENCHMARK_USAGE.md)
for configuration, A/B comparisons and validity checks.

Running `scripts/benchmark/campaign/aggregate_report.py --campaign <campaign.json> --results <results>` also exports UTF-8 `run-inventory.csv`. It contains every discovered formal `runs/**/run-result.json` across smoke, capacity, matrix, soak, recovery and fault scenarios, sorted by relative artifact path. Columns retain run settings, harness exit code, verdict, JSON arrays of missing/failed gates, and `result_path`; absent optional values stay empty. Archived incomplete runs are outside this inventory. Existing summary reports and source artifacts are preserved.

Resource reports accept either plain CSV or gzip CSV via `render_report.py --metrics <file.csv.gz>`. Campaign recovery evidence uses `measurement/metrics.csv`, falling back to `measurement/metrics.csv.gz` only when the plain file is absent. Compression preserves the same columns and calculations; corrupt compressed input remains an error.

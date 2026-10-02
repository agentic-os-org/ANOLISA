# AgentSight CLI Reference

[中文版](../../../zh/agent-observability/agentsight/cli-reference.md)

All flags on this page are taken from `agentsight 0.11.x --help` on Linux. The sample output keeps
the real layout, but every identifier is a placeholder and every count is a round number — none of
it is a real capture.

## Conventions

| Point | Detail |
|---|---|
| Privileges | `trace` needs root (or `CAP_BPF` + `CAP_PERFMON`). Query commands need read access to `/var/log/sysak/.agentsight`, which the packaged service owns — use `sudo`. |
| Config file | Commands that read rules accept `--config`, default `/etc/agentsight/config.json`. `discover` reads the same file `trace` does, so it reflects your custom rules. |
| Data location | Fixed at `/var/log/sysak/.agentsight`. `serve`, `dashboard`, and `skill-metrics` accept `--db` to point at a different database file. |
| Machine output | `--json` is available on `token`, `audit`, `summary`, `interruption *`, and `skill-metrics *`. |
| Output language | `summary`, `metrics`, and `interruption` print English; `discover` and `token` print Chinese regardless of locale. Use `--json` for stable, language-neutral text — note `discover` has no such flag. |
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
trajectory collector, which is pure user-space: it scans local Agent session files (Claude Code,
Qoder, QoderWork, Codex), converts them to ATIF v1.7, and stores them in `trajectories.db` for
`serve` to display.

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

The default scan roots are `/root` and `/home/*`. If the home directory lies elsewhere (a container
using `/app`, for example), set `features.trajectory_collection.scan_dirs` to the session
directories explicitly.

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
        --json           Output as JSON
OPTIONS:
        --last <last>    Query the last N hours (default: 24) [default: 24]
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

## agentsight token

Token consumption for a period, optionally compared with the previous one.

```
FLAGS:
        --compare    Compare with previous period
        --json       Output as JSON

OPTIONS:
        --data-file <data-file>    Custom data file path
        --hours <hours>            Query last N hours
        --period <period>          Query by fixed time period
                                   [possible values: today, yesterday, week, last_week, month, last_month]
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

The running server exposes the same content at `GET /metrics` (localhost only), which is usually
easier to scrape. See [Data and storage](data-and-storage.md#prometheus-metrics).

## agentsight interruption

Queries and resolves detected interruptions. Database:
`/var/log/sysak/.agentsight/interruption_events.db`.

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

# unresolved counts by severity
sudo agentsight interruption count --last 24

# everything that hit one session, then close one event
sudo agentsight interruption session 00000000-1111-2222-3333-444444444444
sudo agentsight interruption resolve 11111111222222223333333344444444
```

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
```

```bash
sudo agentsight skill-metrics all --last 168
sudo agentsight skill-metrics hotness --agent CoshNG --json
```

The same numbers appear on the Dashboard's Skill Metrics page.

## Related pages

- [Configuration](configuration.md) — what the config file controls
- [Dashboard guide](dashboard.md) — UI equivalents of these queries
- [Data and storage](data-and-storage.md) — HTTP API and database layout

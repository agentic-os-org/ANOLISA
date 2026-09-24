# AgentSight Data and Storage

[中文版](../../../zh/agent-observability/agentsight/data-and-storage.md)

Everything AgentSight captures stays on the host in SQLite databases. The Dashboard, the CLI, and
the HTTP API are three views over the same files.

## Where the data lives

All databases sit in `/var/log/sysak/.agentsight/`, created with a private umask so only root can
read them.

| File | Content |
|---|---|
| `genai_events.db` | The main store: LLM calls plus periodic CPU/RSS samples for their Agent processes, with timings, session and conversation IDs |
| `agentsight.db` | Audit records (LLM calls and process actions) and Token consumption aggregates |
| `interruption_events.db` | Detected interruptions with their type, severity, and evidence |
| `optimization.db` | Results of Dashboard optimization analyses |
| `trajectories.db` | ATIF v1.7 trajectories, only when `features.trajectory_collection` is enabled |
| `.agentsight-private/security.db` | Security events, cases, evidence, and containment state |
| `.agentsight-private/enforcement.db` | Enforcement bindings, violations, and transitions |
| `.agentsight-private/reuse.db` | Trajectory reuse labels, human decisions, LLM verdicts, and label audit events |
| `.agentsight-private/causal.db` | Durable causal-attribution cases |
| `.dashboard_token` | The Dashboard access token (64 hex characters, root-only) |
| `optimization_config.json` | LLM settings entered on the Dashboard Settings page (API key stored here) |
| `*.db-wal`, `*.db-shm` | SQLite write-ahead log and shared memory; normal, and checkpointed on clean shutdown |

`--db` on `serve`, `dashboard`, and `skill-metrics` points at a different database file, which is
how you browse a copy or an archive. The tracer itself always writes to the default directory.

> `serve --db <path>` resolves every sibling store from the `--db` directory — GenAI events, the
> interruption store, the trajectory store, and the health checker all follow it. The private
> security, enforcement, reuse, and causal stores follow from its `.agentsight-private/`
> subdirectory. So an archived copy is shown in isolation, without mixing in the live host's data.
> Put the sibling `.db` files and, when present, `.agentsight-private/` directory beside the file you
> pass. A bare relative `--db name.db` uses the current directory.

> These files contain full prompts and model responses. Treat them as sensitive: keep the directory
> permissions as installed, and be careful when copying them off the host.

## Retention and size limits

Schema v4 gives every AgentSight-owned database a `retention_days`, `max_db_size_mb`, and
`check_interval_secs` policy:

| Store | Default policy | Cleanup coverage | Configuration |
|---|---|---|---|
| `agentsight.db` | 30 days, 500 MiB, every 60 seconds | Full: audit, Token, HTTP, and consumption history | `storage.primary` |
| `genai_events.db` | 30 days, 200 MiB, every 60 seconds | Full: GenAI events, resource samples, and evaluation runs share this physical target | `storage.genai` |
| `interruption_events.db` | 30 days, 100 MiB, every 60 seconds | Full interruption history | `storage.interruptions` |
| `trajectories.db` | 30 days, 500 MiB, every 300 seconds | Partial: trajectory rows are pruned while recent skipped-file fingerprints remain protected | `storage.trajectories` |
| `optimization.db` | 30 days, 200 MiB, every 300 seconds | Full optimization-result history | `storage.optimization` |
| `.agentsight-private/security.db` | 30 days, 200 MiB, every 3,600 seconds | Partial: terminal graphs and unreferenced events are pruned; active case graphs remain protected | `storage.security_audit` |
| `.agentsight-private/reuse.db` | 30 days, 200 MiB, every 300 seconds | Partial: old label events and unconfirmed, purely automatic labels only | `storage.reuse` |
| `.agentsight-private/causal.db` | 30 days, 200 MiB, every 300 seconds | Partial: oldest cache entries are evicted, but the latest entry is retained | `storage.causal` |
| `.agentsight-private/enforcement.db` | 30 days, 100 MiB, every 60 seconds | Partial: violations and terminal transitions only | `storage.enforcement` |

Tokenless's `stats.db` is listed by the status API as external. AgentSight opens it read-only and
never applies its lifecycle policy; Tokenless remains responsible for that file.

A zero value disables its corresponding rule: age cleanup, size cleanup, or scheduled checks. An
interval of zero therefore disables automatic governance for that store even if its age and size
values are non-zero. The old `check_interval_inserts` key is unsupported; pre-v4 configuration is
backed up and replaced through the normal schema upgrade mechanism.

Each existing long-running `trace`, `serve`, local trace, or local serve process starts at most one
lightweight `sqlite-maintenance` thread; no separate maintenance process is launched. That thread
runs all of its database jobs sequentially. When `trace` and `serve` both cover one physical file,
they coordinate through `<db>.maintenance.lock`; the process that acquires the lock performs fresh
retention and size measurements before acting.

Every pass follows the same lifecycle:

1. Delete records older than the retention cutoff, subject to the store's schema-safe eligibility rules.
2. If age deletion changed the database, require a successful WAL checkpoint before continuing.
3. Trigger capacity pruning only when physical allocation (database, WAL, and SHM) exceeds the limit.
4. Delete the oldest eligible records and checkpoint between rounds until logical usage reaches 90% of the limit.

Automatic maintenance never runs `VACUUM`. Freed pages remain on the freelist and are reused by
future writes, so a large physical file can be healthy when its logical usage is within target. To
return disk space to the filesystem, stop the service and run
`sudo sqlite3 /var/log/sysak/.agentsight/<db> 'VACUUM;'` manually during a maintenance window.

The partial stores deliberately preserve durable decisions and control state. Reuse maintenance
never deletes human-owned, confirmed, or overridden labels. Enforcement maintenance preserves
bindings, pending or indeterminate transitions, and credential intent/snapshot state. Causal entries
are caches, so eviction can cause a later request to repeat a billed attribution computation.

> For container deployments: retention only matters when the data directory is
> persistent. Without a volume mount, every container restart wipes all data —
> see [Containers and sidecars](deployment.md#containers-and-sidecars).

To change the limits, edit the `storage` section in `/etc/agentsight/config.json` and reload the
service. The Settings page shows the effective policy, physical and logical usage, cleanup coverage,
and maintenance-worker state for every store.

Check current usage from the API:

```bash
TOKEN=$(sudo cat /var/log/sysak/.agentsight/.dashboard_token)
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7396/api/storage/status \
  | python3 -m json.tool
```

The response uses schema version `2`. Each store reports availability, size, policy, coverage, and
`size_state`, plus a `maintenance` object with `scheduled`, `worker_running`,
`worker_heartbeat_unix_ms`, `last_attempt_unix_ms`, `last_success_unix_ms`, `last_result`,
`consecutive_failures`, and `next_run_unix_ms`. Trajectories, security audit, reuse, causal, and
enforcement report `partial` coverage for their protected data. Runtime fields describe only the
process serving this response, so an unscheduled trace-owned store does not prove that another trace
process is stopped. No database path is returned. Treat `within_policy` only as a capacity result:
worker health comes from the scheduling, heartbeat, attempt, result, and failure fields.

You can also inspect the directory directly:

```bash
sudo du -sh /var/log/sysak/.agentsight
sudo ls -la /var/log/sysak/.agentsight
```

## Clearing data

```bash
sudo systemctl stop agentsight.service
sudo rm -rf /var/log/sysak/.agentsight
sudo systemctl start agentsight.service
```

Removing the directory also removes the Dashboard token, so a new one is generated on the next
start. To keep the history instead, copy the directory somewhere safe and browse it later with
`agentsight serve --db /path/to/genai_events.db`.

## HTTP API

The server publishes its own route inventory, so you never have to guess:

```bash
curl -s http://127.0.0.1:7396/api/docs | python3 -m json.tool
```

Requests from anywhere other than loopback need the token:

```bash
TOKEN=$(sudo cat /var/log/sysak/.agentsight/.dashboard_token)
curl -s -H "Authorization: Bearer $TOKEN" http://<host>:7396/api/sessions
```

Endpoint groups in 0.11:

| Group | Examples | Purpose |
|---|---|---|
| Service | `GET /health`, `GET /metrics`, `GET /api/docs` | Liveness, Prometheus metrics, route list (`/health` and `/metrics` are loopback-only) |
| Authentication | `GET /api/auth/status`, `GET /api/auth/verify`, `POST /api/auth/login` | Auth state, capability list, token → cookie exchange |
| Sessions and traces | `GET /api/sessions`, `GET /api/sessions/{id}/traces`, `GET /api/sessions/{id}/resources`, `GET /api/traces/{id}`, `GET /api/conversations/{id}`, `POST /api/sessions/search` | Session list, per-session conversation summaries (keyed by `conversation_id`) and process resources, per-call detail by response id, semantic search |
| Metrics | `GET /api/timeseries`, `GET /api/metrics/latency`, `GET /api/agent-names` | Token time series, latency percentiles, Agent filter values |
| Interruptions | `GET /api/interruptions`, `/count`, `/stats`, `/session-counts`, `/conversation-counts`, `POST /api/interruptions/{id}/resolve` | Triage and resolution |
| Agent health | `GET /api/agent-health`, `DELETE /api/agent-health/{pid}`, `POST /api/agent-health/{pid}/restart` | Live Agent state and recovery actions |
| Token savings | `GET /api/token-savings`, `GET /api/token-savings/session/{id}` | Tokenless savings |
| ATIF export | `GET /api/export/atif/session/{id}` (also `trace` and `conversation`) | Trajectory export |
| Trajectories | `GET /api/trajectories`, `/filters`, `/steps`, `/{session_id}` | Collected trajectories. The list accepts optional `label`, `exclude_label`, and `human_backed` filters; `label` is comma-separated effective labels such as `good,bad` |
| Reuse labels | `POST /api/reuse/triage`, `GET /api/reuse/sessions`, `POST /api/reuse/sessions/{session_id}/label`, `POST /api/reuse/sessions/labels:batch-confirm`, `GET /api/reuse/label-stats`, `POST /api/reuse/judge` | Rule triage and human label decisions. The judge requires `features.reuse_llm_judge=true` and configured LLM credentials; it makes billed model calls |
| Preferences | `GET /api/preferences`, `/export`, `/turns` | User preference analysis, Markdown export, and source user turns for agent-side reasoning |
| Storage | `GET /api/storage/status` | Schema-v2 policy, capacity, coverage, and maintenance-worker status for every SQLite target; paths are not returned |
| Skill metrics | `GET /api/skill-metrics`, `/downloads`, `/loads`, `/usage-ratio`, `/distribution`, `/hotness` | Skill adoption |
| Optimization | `POST /api/optimize/sessions/{id}/{dimension}`, `GET /api/optimize/results`, `GET` and `POST /api/optimize/config` | LLM-assisted analysis |
| Quality and attribution | `POST /api/grader/evaluate`, `GET /api/grader/latest`, `POST /api/causal-attribution` | Session quality scoring, root-cause attribution |
| Security and audit | `GET /api/security/*`, `GET /api/audit/*`, `POST /api/audit/cases/{id}/review` | Present when agent-sec-core is installed |
| Enforcement | `GET /api/enforcement/health`, `POST /api/enforcement/bindings`, `GET /api/enforcement/violations` | Present when the enforcer is installed; mutations always require the token |

Time ranges are nanosecond epochs (`start_ns`, `end_ns`), matching the CLI's `--last` window.

```bash
# last hour of sessions
NOW=$(date +%s%N); AGO=$((NOW - 3600000000000))
curl -s "http://127.0.0.1:7396/api/sessions?start_ns=$AGO&end_ns=$NOW" | python3 -m json.tool | head
```

Fetch the raw CPU/RSS points and activity intervals for one Session:

```bash
curl -s "http://127.0.0.1:7396/api/sessions/<SESSION_ID>/resources?max_points=2000" | python3 -m json.tool
```

Each sample is process-level and contains an epoch-nanosecond timestamp, PID, CPU percentage, and
resident memory in bytes. For a multi-process Agent, the Dashboard sums the PIDs associated with
the Session. This is execution-context data rather than strict per-Session attribution: a shared
Agent process can serve more than one Session at the same time. LLM intervals use captured request
and response timestamps; a Tool Call interval is inferred from the LLM response that requested the
tool to the next LLM request carrying the matching tool result. Gaps not covered by an LLM call or
a matched Tool Call are returned as `idle`; unmatched Tool Calls are not assigned a fabricated end
timestamp.

## Prometheus metrics

```bash
curl -s http://127.0.0.1:7396/metrics | head
```

```
# HELP agentsight_token_input_total Total input tokens consumed by agent (all-time)
# TYPE agentsight_token_input_total counter
agentsight_token_input_total{agent="CoshNG"} 100000
agentsight_token_input_total{agent="Cosh"} 50000
```

Counters are per Agent and all-time: `agentsight_token_input_total`,
`agentsight_token_output_total`, `agentsight_token_total_total`, `agentsight_llm_requests_total`.
`/metrics` is loopback-only, so scrape it with a node-local Prometheus agent or expose it through a
local reverse proxy. `agentsight metrics` prints the same content from the CLI.

## Trajectory export (ATIF v1.7)

Any session, conversation, or single trace exports as a self-contained JSON trajectory — Agent
metadata, steps, messages, tool calls, and Token totals:

```bash
curl -s http://127.0.0.1:7396/api/export/atif/session/<SESSION_ID> > session.atif.json
```

The Dashboard's Trajectory Viewer offers the same file through **Download JSON**, and can re-import
one captured on another host. Use it for offline analysis, sharing a reproduction, or feeding
evaluation pipelines.

## External log export

AgentSight can write structured events to a file for an external log collector to pick up:

```json
{
  "runtime": { "sls_logtail_path": "/var/log/anolisa/agentsight/events.jsonl" },
  "features": { "sls_logtail": true }
}
```

The path can be changed while AgentSight runs — set it to `""` to pause export. Leave both settings
at their defaults if you want the data to stay entirely local. Collector-side configuration
(endpoints, credentials) is out of scope for AgentSight.

## Backup

```bash
sudo systemctl stop agentsight.service
sudo tar czf agentsight-data-$(date +%F).tar.gz -C /var/log/sysak .agentsight
sudo systemctl start agentsight.service
```

Stopping the service first ensures the WAL is checkpointed, so the archive is consistent.

## Related pages

- [CLI reference](cli-reference.md) — query the same data from the shell
- [Dashboard guide](dashboard.md) — the UI over these databases
- [Configuration](configuration.md) — storage and retention switches

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
| `.dashboard_token` | The Dashboard access token (64 hex characters, root-only) |
| `optimization_config.json` | LLM settings entered on the Dashboard Settings page (API key stored here) |
| `*.db-wal`, `*.db-shm` | SQLite write-ahead log and shared memory; normal, and checkpointed on clean shutdown |

`--db` on `serve`, `dashboard`, and `skill-metrics` points at a different database file, which is
how you browse a copy or an archive. The tracer itself always writes to the default directory.

> `serve --db <path>` resolves every sibling store from the `--db` directory — GenAI events, the
> interruption store, the trajectory store, and the health checker all follow it. So an archived
> copy is shown in isolation, without mixing in the live host's data. Put the sibling `.db` files in
> the same directory as the one you pass. A bare relative `--db name.db` uses the current directory.

> These files contain full prompts and model responses. Treat them as sensitive: keep the directory
> permissions as installed, and be careful when copying them off the host.

## Retention and size limits

| Store | Limit | How to change it |
|---|---|---|
| `genai_events.db` | 200 MB by default; pruning starts at 90% of the cap and removes the oldest LLM calls and process-resource samples | `AGENTSIGHT_GENAI_DB_MAX_SIZE_MB=500` in the service environment |
| `interruption_events.db` | 30 days and 100 MB | `features.interruption_detection.retention_days` / `max_db_size_mb` |

The limit is a logical-data cap (physical file size minus free pages); pruning
removes the oldest records first until the logical size fits. The physical file
does not shrink after pruning: freed pages go to the freelist and are reused by
future writes, so the file stabilizes at its historical peak. To return disk
space to the filesystem, run `sudo sqlite3 /var/log/sysak/.agentsight/<db>
'VACUUM;'` manually during a low-traffic window — VACUUM rebuilds the whole
file, so stop the service first to avoid tripping the cgroup memory limit.

> For container deployments: retention only matters when the data directory is
> persistent. Without a volume mount, every container restart wipes all data —
> see [Containers and sidecars](deployment.md#containers-and-sidecars).

To raise the GenAI cap for the packaged service:

```bash
sudo systemctl edit agentsight.service
# [Service]
# Environment=AGENTSIGHT_GENAI_DB_MAX_SIZE_MB=500
sudo systemctl restart agentsight.service
```

Check current usage:

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
| Trajectories | `GET /api/trajectories`, `/filters`, `/{session_id}` | Collected trajectories |
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

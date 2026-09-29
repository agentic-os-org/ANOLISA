# AgentSight Configuration

[中文版](../../../zh/agent-observability/agentsight/configuration.md)

AgentSight reads one JSON file: `/etc/agentsight/config.json` (override with `--config`).
It controls which processes are traced, which features run, and how much memory the pipeline may
use. The reference copy shipped with the source is `src/agentsight/agentsight.json`.

## Two rules to know before you edit

1. **Your file replaces the built-in defaults; it does not extend them.** If your `cmdline.allow`
   list omits a rule, that Agent is no longer discovered. Always start from the shipped file and
   add to it.
2. **Reload instead of restart.** After editing, run `sudo systemctl reload agentsight.service`.
   The supervisor restarts both workers so they re-read the file; tracing resumes within seconds.

## File layout

```json
{
  "schema_version": 4,
  "storage": {
    "base_path": "/var/log/sysak/.agentsight",
    "primary": { "retention_days": 30, "max_db_size_mb": 500, "check_interval_secs": 60 },
    "genai": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 60 },
    "interruptions": { "retention_days": 30, "max_db_size_mb": 100, "check_interval_secs": 60 },
    "trajectories": { "retention_days": 30, "max_db_size_mb": 500, "check_interval_secs": 300 },
    "optimization": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 300 },
    "security_audit": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 3600 },
    "reuse": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 300 },
    "causal": { "retention_days": 30, "max_db_size_mb": 200, "check_interval_secs": 300 },
    "enforcement": { "retention_days": 30, "max_db_size_mb": 100, "check_interval_secs": 60 }
  },
  "runtime": {
    "sls_logtail_path": ""
  },
  "server": {
    "auth": { "enabled": true }
  },
  "deadloop": {
    "enabled": false,
    "kill_after_count": 3
  },
  "features": {
    "token_stats": true,
    "tokenizer": { "enabled": false, "cache_size": 4 },
    "session_mapping": { "enabled": true, "max_entries": 10000 },
    "sqlite_storage": { "enabled": true, "batch": { "max_size": 100, "flush_ms": 100 } },
    "resource_sampling": false,
    "interruption_detection": { "enabled": true },
    "audit": true,
    "token_consumption": false,
    "sls_logtail": false,
    "trajectory_collection": { "enabled": false, "scan_interval_secs": 30 },
    "reuse_llm_judge": false
  },
  "runtime_limits": {
    "event_channel_capacity": 10000,
    "event_channel_policy": "backpressure",
    "event_channel_max_bytes_mb": 64,
    "pending_genai_max_count": 1000,
    "pending_genai_max_bytes_mb": 64,
    "pid_cache_size": 1024,
    "max_connection_body_mb": 8,
    "connection_idle_timeout_secs": 60,
    "ring_buffer_mb": 32
  },
  "https": [
    { "rule": ["dashscope.aliyuncs.com"] },
    { "rule": ["api.openai.com"] }
  ],
  "http": [],
  "cmdline": {
    "allow": [
      { "rule": ["*cosh-core*"], "agent_name": "CoshNG" },
      { "rule": ["*node*", "*claude*"], "agent_name": "Claude" }
    ],
    "deny": [
      { "rule": ["*", "*", "-c", "*sftp-server*"] }
    ]
  },
  "codex_offsets": { "schema_version": 1, "entries": [] }
}
```

## SQLite storage policies

`storage.base_path` is the directory shared by AgentSight-owned databases. In schema v4, every
policy uses the same keys: `retention_days`, `max_db_size_mb`, and `check_interval_secs`. A zero
value disables the corresponding age-retention, capacity, or scheduled-maintenance rule. If
`check_interval_secs` is `0`, no automatic pass is scheduled even when the other two values are
non-zero.

| Store | Retention | Size limit | Check interval |
|---|---:|---:|---:|
| `storage.primary` (`agentsight.db`) | 30 days | 500 MiB | 60 seconds |
| `storage.genai` (`genai_events.db`, including evaluations) | 30 days | 200 MiB | 60 seconds |
| `storage.interruptions` | 30 days | 100 MiB | 60 seconds |
| `storage.trajectories` | 30 days | 500 MiB | 300 seconds |
| `storage.optimization` | 30 days | 200 MiB | 300 seconds |
| `storage.security_audit` | 30 days | 200 MiB | 3,600 seconds |
| `storage.reuse` | 30 days | 200 MiB | 300 seconds |
| `storage.causal` | 30 days | 200 MiB | 300 seconds |
| `storage.enforcement` | 30 days | 100 MiB | 60 seconds |

Maintenance first removes expired eligible rows. If that changed the database, a successful WAL
checkpoint gates the capacity phase. Physical allocation above `max_db_size_mb` triggers pruning;
the pass deletes the oldest eligible records until logical usage is at most 90% of the limit. It
does not run `VACUUM`, so freed pages remain on the SQLite freelist for later writes.

`check_interval_inserts` is no longer supported. The configuration format is replaced as a unit when
`schema_version` changes: a missing or pre-v4 version is backed up and replaced with schema v4
instead of merging old settings. Start custom files from the shipped `agentsight.json` and do not
copy the old key into the replacement.

## Feature switches

Everything under `features` can be turned off independently. A disabled feature is not
instantiated at all, so it costs no memory and no I/O.

| Feature | JSON path | Default | Effect |
|---|---|---|---|
| Token accounting | `features.token_stats` | `true` | Core capability: per-Agent, per-model Token counting |
| Local tokenizer | `features.tokenizer.enabled` | `false` | Hugging Face tokenizer fallback when the provider returns no usage block |
| Session mapping | `features.session_mapping.enabled` | `true` | Maps provider response IDs to Agent session IDs |
| SQLite storage | `features.sqlite_storage.enabled` | `true` | Local persistence; off means a no-op store and an empty Dashboard |
| Resource sampling | `features.resource_sampling` | `false` | Samples Agent CPU/RSS once per second; requires SQLite storage |
| Interruption detection | `features.interruption_detection.enabled` | `true` | Detects failures, stalls, and loops |
| Audit | `features.audit` | `true` | Persists LLM calls and process actions |
| Token consumption records | `features.token_consumption` | `false` | Extra aggregated consumption records |
| External log export | `features.sls_logtail` | `false` | Writes structured events to a log file for an external collector |
| Trajectory collection | `features.trajectory_collection.enabled` | `false` | Periodically scans local Agent JSONL sessions into `trajectories.db` (trace mode only) |
| Reuse LLM judge | `features.reuse_llm_judge` | `false` | Allows `POST /api/reuse/judge` to ask the configured LLM to label trajectories the rules cannot place; every call is billed |

`reuse_llm_judge` affects only `POST /api/reuse/judge`. Leave it disabled unless you have configured an optimization LLM and explicitly want paid second-level labelling. Reload the service after changing it so the server reads the new setting.

Tuning knobs that come with a feature:

| Setting | Default | Meaning |
|---|---|---|
| `features.tokenizer.cache_size` | `4` | Number of tokenizer models kept in memory |
| `features.session_mapping.max_entries` | `10000` | Bound of the response-ID → session-ID map |
| `features.sqlite_storage.batch.max_size` | `100` | Rows per write batch |
| `features.sqlite_storage.batch.flush_ms` | `100` | Maximum batch delay in milliseconds |
| `features.trajectory_collection.scan_interval_secs` | `30` | Scan interval for discovering trajectory files; storage cleanup uses `storage.trajectories.check_interval_secs` |

## Runtime limits

These bound every in-memory buffer in the pipeline. Raise them for very busy hosts; lower them for
memory-constrained ones. `MemoryMax=350M` in the packaged systemd unit assumes roughly the defaults.

| Setting | Default | Meaning |
|---|---|---|
| `event_channel_capacity` | `10000` | Bounded probe → pipeline channel |
| `event_channel_policy` | `backpressure` | Behaviour when the channel is full: `backpressure`, `drop_newest`, `sample` |
| `event_channel_max_bytes_mb` | `64` | Byte budget for events queued in that channel; `0` disables it. Enforced alongside the slot count, because one captured SSL record can be up to 4 MiB — 10 000 slots alone do not bound memory. Events arriving over budget are dropped and counted in the log |
| `pending_genai_max_count` | `1000` | Events waiting for a session ID |
| `pending_genai_max_bytes_mb` | `64` | Byte cap for the same queue |
| `pid_cache_size` | `1024` | PID → Agent name LRU entries |
| `max_connection_body_mb` | `8` | Body buffer per HTTP connection |
| `connection_idle_timeout_secs` | `60` | Idle timeout before a connection buffer is dropped |
| `ring_buffer_mb` | `32` | eBPF ring buffer size; must be a power of two |

## Agent discovery rules

`cmdline.allow` decides which processes count as Agents and what name they get. Each rule is a list
of command-line tokens with `*` wildcards; all tokens must match in order.

```json
{ "rule": ["*node*", "*claude*"], "agent_name": "Claude" }
```

matches a process whose first argument contains `node` and whose next argument contains `claude`.

The shipped file covers Hermes, Codex, Runloop, cosh (`Cosh`), cosh-ng (`CoshNG`), OpenClaw,
Claude Code, Qwen Code, and AgentScope — 31 rules in total.

To add your own Agent, append a rule and reload:

```json
{ "rule": ["*python*", "*my_agent*"], "agent_name": "MyAgent" }
```

```bash
sudo systemctl reload agentsight.service
```

Then confirm the rule is live:

```bash
sudo agentsight discover --list-known | grep -i MyAgent   # rule now listed
sudo agentsight discover                                   # run your Agent, then see it matched
```

`discover` reads the same `config.json` the tracer uses, so `--list-known` reflects your addition
(pass `--config <path>` to check a different file). You can also confirm through captured data —
`sudo agentsight summary --last 1` should show non-zero sessions once the Agent runs; check
`journalctl -u agentsight.service` if nothing arrives.

`cmdline.deny` removes matches that would otherwise be traced — the default entry keeps
`sftp-server` subprocesses out of the data.

Two practical points:

- A Rust or Go Agent binary is not matched by `node*`-style rules. Add a rule for the binary name.
- Wrapper processes matter. cosh-ng spawns `cosh-shell` and `cosh-core`, so both have rules.

## Endpoint rules

| Section | Purpose |
|---|---|
| `https` | Domains whose TLS traffic is decrypted through uprobes. Add your provider domain here if it is missing. |
| `http` | Plaintext HTTP targets captured through the TCP probe. |

```json
"https": [
  { "rule": ["dashscope.aliyuncs.com"] },
  { "rule": ["api.openai.com"] },
  { "rule": ["my-gateway.internal"] }
]
```

## Dashboard authentication

```json
"server": { "auth": { "enabled": true } }
```

Token authentication is on by default. Loopback requests skip it; remote requests need the token.
Set `enabled` to `false` only on a trusted internal network — see
[Dashboard guide](dashboard.md#authentication).

## Dead-loop auto-stop

```json
"deadloop": { "enabled": false, "kill_after_count": 3 }
```

Off by default. When enabled, AgentSight terminates an Agent process after it repeats the same
tool-call loop `kill_after_count` times. Detection and reporting of dead loops work regardless;
this switch only controls whether AgentSight acts. See
[Interruption detection](interruption-detection.md#dead-loop-handling).

## External log export

```json
"runtime": { "sls_logtail_path": "" },
"features": { "sls_logtail": false }
```

A non-empty `runtime.sls_logtail_path` activates file-based export of structured events for an
external log collector, and the path can be changed while AgentSight is running. Leave it empty to
keep everything local. See [Data and storage](data-and-storage.md#external-log-export).

## Codex offsets

`codex_offsets` holds per-version symbol offsets for Codex CLI, which statically links its TLS
library and exports no symbols. AgentSight tries the symbol table, then byte-pattern matching, then
this table. If a new Codex release is not captured, regenerate the entry with
`src/agentsight/scripts/extract-codex-offsets.py`.

## schema_version and upgrades

`schema_version` (currently `4`) marks the config format. On start, AgentSight compares it with the
built-in version:

- equal or newer → your file is left untouched;
- missing or older → AgentSight copies your file to `config.json.bak.<unix-seconds>` and replaces it
  with the current default file.

Schema v4 replaces insert-count storage checks with `check_interval_secs` for every policy and adds
`reuse`, `causal`, and `enforcement`. Custom settings from an older schema are not merged. Reapply
required custom rules to the new file, then reload the service.

## Environment variables

| Variable | Purpose |
|---|---|
| `AGENTSIGHT_TOKENIZER_PATH` | Directory holding local tokenizer models |
| `AGENTSIGHT_ENFORCER_SOCKET` | Enforcer socket path (default `/run/agentsight/enforcer.sock`) |
| `AGENTSIGHT_CHROME_TRACE` | Writes a Chrome trace file for pipeline profiling |
| `RUST_LOG` | Log level, e.g. `RUST_LOG=debug` |

## Verify a change

```bash
sudo systemctl reload agentsight.service
systemctl is-active agentsight.service
sudo agentsight summary --last 1
```

If the service refuses to come back, the file is usually invalid JSON:

```bash
python3 -m json.tool /etc/agentsight/config.json > /dev/null && echo "JSON ok"
journalctl -u agentsight.service -n 30 --no-pager
```

---
name: redis-admin
version: 0.1.0
description: Use this skill when operating a running Redis (or Valkey) instance - diagnosing server health and client connections, investigating latency spikes and slow commands, governing memory usage and eviction behavior, hunting big keys with bounded SCAN iteration (never KEYS *), checking RDB/AOF persistence and fork health, and verifying replication topology and lag in read-only mode. Triggers on redis-cli sessions, INFO section analysis, CLIENT LIST and blocked-client checks, SLOWLOG and LATENCY inspection, memory fragmentation and evicted_keys investigation, BGSAVE/BGREWRITEAOF status checks, INFO replication and SENTINEL masters observation.
layer: application
lifecycle: production
category: devtools
tags: [redis, valkey, cache, in-memory, replication, persistence, memory, troubleshooting]
dependencies: [ktuner]
platforms:
  - claude-code
  - gemini-cli
  - openai-codex
license: Apache-2.0
maintainers:
  - github: Zzzzzmeow
---

# Redis Instance Administration

Operation of a running Redis 7.x instance (with Valkey equivalents where they
apply). This skill covers the safety-critical day-to-day work of a Redis
operator: reading server health, finding latency spikes and slow commands,
governing memory and eviction, hunting big keys without stalling the server,
verifying persistence health (RDB snapshots, AOF rewrites, fork costs), and
checking replication and Sentinel state in observation-only mode. The
instance is treated as a production asset: read-only inspection comes first,
every write or flush action is listed and confirmed before it runs, and no
destructive command (`FLUSHALL`, `FLUSHDB`, `DEBUG ...`) is ever issued as a
"quick fix".

---

## When to use this skill

Trigger this skill when the user:

- Asks what a Redis server is doing: INFO sections, connected clients,
  blocked clients, command throughput
- Reports latency spikes or wants slow command and event-loop analysis
  (`SLOWLOG`, `LATENCY`)
- Suspects memory pressure: `used_memory` vs `maxmemory`, eviction storms,
  key expiration backlog, fragmentation ratio
- Wants to find big keys or key-space composition - safely, via bounded
  `SCAN` cursor iteration
- Asks about persistence health: RDB snapshot status, AOF rewrite failures,
  fork latency (`latest_fork_usec`), disk space under the persistence dir
- Wants to verify replication topology, replica lag, or `master_link_status`
- Wants to observe Sentinel failover state (read-only) without triggering
  anything
- Asks whether key settings (`maxmemory-policy`, `appendonly`, appendfsync)
  are sensible for the workload

Do NOT trigger this skill for:

- Installing, upgrading, or initially configuring a Redis server -
  deployment topic, covered by distribution package management and initial
  provisioning, not this skill
- Redis Cluster topology work (slot migration, `RESHARD`, adding/removing
  nodes, `CLUSTER SET-CONFIG-EPOCH`) - cluster orchestration is a separate
  change-management topic; this skill covers single-instance and
  primary/replica + Sentinel topologies only
- Kernel and memory tuning of the host (`vm.overcommit_memory`, THP, ulimit
  rework, sysctl changes) - the `ktuner` skill owns host-level tuning; this
  skill only reports current values and their Redis-visible symptoms
- Bulk application data migration or cache warm-up orchestration - the
  application owns key schema and rollout sequencing
- Managed cloud Redis instances (ElastiCache, Alibaba Cloud Tair/RDS) where
  vendor consoles expose diagnostics differently - do not assume `CONFIG`
  or `DEBUG` commands exist there

---

## Key principles

1. **Read-only first** - Diagnose with `INFO`, `CONFIG GET`, `CLIENT LIST`,
   `SLOWLOG GET`, `LATENCY HISTORY`, `DBSIZE`, and `SCAN` before touching
   anything. The safe default for every investigation is a read-only
   command; writes require an explicit reason.
2. **List before killing or changing** - Before `CLIENT KILL`,
   `CONFIG SET`, `MEMORY PURGE`, `FLUSHDB`, or any data-deleting command,
   list the exact affected objects (client IDs, current vs proposed value,
   keyspace scope) and get user confirmation.
3. **No interactive sessions** - Agent runtimes are non-interactive. Never
   open a bare `redis-cli` shell or `MONITOR` stream; run every check as
   one bounded invocation: `redis-cli --raw <command> | head -N`. `MONITOR`
   additionally degrades the server and streams unboundedly - refuse it.
4. **Never guess credentials or paths** - Connect with whatever the
   environment provides (`redis-cli ping` on localhost:6379, a unix socket,
   or `REDISCLI_AUTH` supplied by the user). If authentication fails, ask
   the user how they authenticate; do not try password combinations and
   never pass `-a <password>` on the command line where `ps` can see it.
   Locate the persistence dir and logfile with `CONFIG GET dir` /
   `CONFIG GET logfile`, and the config file itself via `INFO server`
   (`config_file` field), not by assuming `/var/lib/redis`.
5. **Redis 7.x semantics** - The commands below assume Redis 7.x
   (`redis_version:7.*`): `CONFIG GET` returns key/value pairs as array
   items, `MEMORY USAGE`, `LATENCY HISTORY`, function libraries (`FUNCTION
   LIST`) exist, and ACLs (`ACL WHOAMI`, `ACL GETUSER`) are available. On
   Valkey or older Redis, substitute equivalents (Valkey 7/8 is
   command-compatible with Redis 7) rather than assuming identical output.
6. **Bounded output** - `INFO` sections, `CLIENT LIST` on busy servers, and
   `SLOWLOG` can be large. Pipe through `head`, grep specific fields, or
   request a single `INFO <section>`; never dump full command stats or the
   whole keyspace into the conversation. `KEYS *` is forbidden outright -
   it is O(N) and blocks the single-threaded event loop; use `SCAN` with a
   cursor and `COUNT` hint instead.

---

## Step zero: probe the environment

Run these read-only checks before any other action:

```bash
# Client availability and version
command -v redis-cli
redis-cli --version

# Connectivity and auth - try the zero-config path first (localhost:6379,
# or the host/port the user named). A NOAUTH/AUTH error here is a question
# to the user, not a password guessing exercise.
redis-cli --raw PING

# If that fails, check whether a local server exists before asking anything
command -v redis-server systemctl 2>/dev/null
systemctl is-active redis redis-server valkey 2>/dev/null

# Server facts that shape every later decision (read from the server,
# not from config paths)
redis-cli --raw INFO server | grep -E 'redis_version|redis_mode|os:|run_id|uptime_in_seconds'
redis-cli --raw INFO replication | grep -E '^role|connected_slaves'
redis-cli --raw CONFIG GET maxmemory maxmemory-policy appendonly appendfsync
```

`redis_mode` matters immediately: `cluster` mode means this skill's scope
rules apply (observe only, no topology commands); `sentinel` mode on port
26379 means every check is read-only observation of masters and replicas.
If auth is required, ask the user for `REDISCLI_AUTH` or a config file -
never `-a` on the command line.

If `CONFIG` is unavailable (`NOPERM`, or `ERR unknown command 'CONFIG'`
because it was renamed or ACL-denied - common on hardened or managed
instances), take the same facts from `INFO` instead: `INFO server` carries
`config_file` and `executable`, `INFO memory` carries `maxmemory` and
`maxmemory_policy`, `INFO persistence` carries the appendonly/fsync state.
Degrade to those and ask the user for the config file path; do not treat
the missing command as an obstacle to work around.

---

## Runtime diagnostics: server health, clients, latency

```bash
# Core health snapshot - one INFO section at a time, grepped
redis-cli --raw INFO stats | grep -E 'total_connections_received|total_commands_processed|instantaneous_ops_per_sec|rejected_connections|evicted_keys|expired_keys'
redis-cli --raw INFO clients | grep -E 'connected_clients|blocked_clients|maxclients'
redis-cli --raw INFO cpu | grep -E 'used_cpu_sys|used_cpu_user'

# Connected clients (bounded; TYPE normal filters server-side on 6.2+,
# excluding replica/pubsub connections; head is the bound on older servers)
redis-cli --raw CLIENT LIST TYPE normal | head -40

# Idle-heavy clients holding connections open
redis-cli --raw CLIENT LIST TYPE normal | awk '{for (i=1; i<=NF; i++) if ($i ~ /^idle=/) {split($i, a, "="); if (a[2]+0 > 300) print}}' | head -20
```

**Blocked clients** (`blocked_clients` > 0 in `INFO clients`) usually means
`BLPOP`/`BRPOP`/`XREAD BLOCK` waiters - normal for queue workloads - or
`WAIT`/`SYNC` stalls. Distinguish by looking at the client `cmd` field in
`CLIENT LIST` before proposing anything.

**Latency events** (requires `latency-monitor-threshold` > 0; check it
first, and note that enabling it via `CONFIG SET` is a write action):

```bash
redis-cli --raw CONFIG GET latency-monitor-threshold
# If enabled: list events, then inspect history per event
redis-cli --raw LATENCY HISTORY event-name
redis-cli --raw LATENCY LATEST
```

**Slow commands** - bounded, newest first:

```bash
redis-cli --raw CONFIG GET slowlog-log-slower-than slowlog-max-len
redis-cli --raw SLOWLOG GET 10
```

If the slowlog is disabled (`slowlog-log-slower-than = 0` means off; a
negative value logs everything - check before interpreting), do not flip it
silently - enabling it is a server change (propose the `CONFIG SET` with
its cost and let the user decide). Prefer read-only alternatives while it
is off: `INFO commandstats` for aggregate command costs.

```bash
redis-cli --raw INFO commandstats | head -30
```

**Killing a client is a write action**: `CLIENT KILL ID <id>` (and
`CLIENT PAUSE`-class interventions that freeze all client writes - see
the confirmation list). Follow the list-before-change rule:
show the client's address, age, idle time, and last command, then act on
confirmation. Killing a replication connection (`CLIENT KILL TYPE replica`)
desyncs replicas - treat it as a topology change.

---

## Memory governance: usage, eviction, fragmentation, big keys

```bash
# The memory picture in one bounded view
redis-cli --raw INFO memory | grep -E 'used_memory_human|used_memory_rss_human|maxmemory_human|maxmemory_policy|mem_fragmentation_ratio|mem_allocator|total_system_memory_human'

# Eviction and expiration pressure (trend, not snapshot: sample twice)
redis-cli --raw INFO stats | grep -E 'evicted_keys|expired_keys|keyspace_hits|keyspace_misses'

# Keyspace composition (small: one line per DB)
redis-cli --raw INFO keyspace
redis-cli --raw DBSIZE
```

**Eviction triage**: `evicted_keys` growing means the dataset no longer
fits `maxmemory` under the current `maxmemory_policy`. Report the policy
semantics (`allkeys-lru` evicts anything, `volatile-lru` only TTL keys -
if no keys carry TTLs, `volatile-*` policies degenerate into OOM errors at
`maxmemory`), the hit/miss ratio (a cache with falling hit rate plus rising
evictions is undersized), and present resize options (`CONFIG SET
maxmemory` - a confirmed write; a real fix is capacity planning, not this
skill).

**Fragmentation**: `mem_fragmentation_ratio` (used_memory_rss /
used_memory) above ~1.5 with stable memory means fragmentation or a fork
(COW) in progress; below 1.0 means the OS has swapped Redis memory -
escalate to the `ktuner`/host level rather than tuning Redis. `MEMORY
PURGE` (jemalloc) is a write-class action - list it, don't run it.

**Big keys - bounded SCAN iteration, never `KEYS *`**:

```bash
# First pass: redis-cli's samplers. SCAN-based, but they traverse the
# WHOLE keyspace - on a large DB expect minutes, not seconds. Throttle
# with -i (pause per command cycle) on busy servers, and keep only the
# per-type summary in the conversation.
redis-cli --bigkeys -i 0.01 | tail -20   # per-type biggest keys, sampled
redis-cli --memkeys -i 0.01 | tail -20   # memory-focused variant

# Manual pass when the sampler's 10-iteration default is not enough:
redis-cli --raw SCAN 0 COUNT 1000        # returns cursor + batch of keys

# Per-key size during the survey: default sampling (~5 elements per
# collection value) - cheap, approximate, safe to loop over candidates
redis-cli --raw MEMORY USAGE somekey
```

`MEMORY USAGE <key> SAMPLES 0` is the *exact* mode - Redis walks the
entire collection to report the true size, O(N) on the single-threaded
event loop, and typically on exactly the keys this section hunts. Use it
only for one already-identified key, after `SCARD`/`HLEN`/`LLEN`/`XLEN`
shows the cardinality, and treat its cost like a write-class action on
big collections. Never put `SAMPLES 0` inside a loop over many keys.

When iterating manually, bound the loop: at most ~50 batches of
`COUNT 1000` or ~30 seconds wall-clock, whichever comes first. If the
cursor has not returned to 0 by then, stop, report the partial coverage
and ask the user whether to continue. Batch `MEMORY USAGE` checks with
default sampling, reporting only keys above a threshold (e.g. 1 MB).
Never `KEYS *`, never `SMEMBERS` on a suspect big key (it materializes
the whole set) - use `SCARD`/`MEMORY USAGE` first, and `SSCAN`/`HSCAN`
with `COUNT` for bounded inspection of the contents.

Dropping or splitting a big key is an application-schema decision and a
write action (`UNLINK` over `DEL` - it frees asynchronously off the event
loop). List the key, its type, its memory, and the `UNLINK` proposal;
act on confirmation only.

---

## Persistence and fork health

```bash
# RDB and AOF status in one view
redis-cli --raw INFO persistence | grep -E 'rdb_last_save_time|rdb_last_bgsave_status|rdb_changes_since_last_save|rdb_last_bgsave_time_sec|aof_enabled|aof_last_write_status|aof_last_bgrewrite_status|aof_current_size|aof_last_write_size|loading:'

# Fork cost - the single most common latency source on save/rewrite
redis-cli --raw INFO stats | grep -E 'latest_fork_usec|total_forks'

# Persistence location and disk space (from the server, not a guessed path)
redis-cli --raw CONFIG GET dir dbfilename appendfilename appenddirname appendfsync
# Validate before shell substitution: an auth/permission error leaves
# stdout empty or an error string, and a blind $(...) hides the real cause
dir="$(redis-cli --raw CONFIG GET dir | tail -1)"
case "$dir" in /*) df -h "$dir" ;; *) echo "could not resolve persistence dir (CONFIG unavailable?)" ;; esac
```

Reading it:

- `rdb_last_bgsave_status: err` means the last `BGSAVE` failed - check the
  logfile path (`CONFIG GET logfile`) and disk space before anything else;
  a failed snapshot is silent data-risk, report it prominently. With the
  default `stop-writes-on-bgsave-error yes`, every write is then rejected
  with `MISCONF` - the write outage and the save failure are the same
  incident. Fix the save failure; never just disable the guard.
- `aof_last_write_status: err` (or `aof_last_bgrewrite_status: err`) means
  AOF writes are failing - on `appendfsync everysec` this also means
  potential fsync stalls (`INFO persistence: aof_delayed_fsync`).
- `latest_fork_usec` above ~100ms (rule of thumb: fork cost grows with
  used_memory) predicts event-loop stalls on every `BGSAVE`/`BGREWRITEAOF`;
  the host-level fix (THP, overcommit) belongs to `ktuner` - report, don't
  touch.
- `rdb_changes_since_last_save` large and growing means the RDB is stale -
  recovery-point risk. `BGSAVE` is a write-class action (fork cost, disk
  I/O): propose it, don't fire it unprompted.

**Dangerous persistence actions - refuse or gate**:

- `SAVE` (synchronous dump) blocks the server for the full dump duration -
  refuse on production; use `BGSAVE` (proposed, confirmed).
- `FLUSHALL` / `FLUSHDB` destroy data irrecoverably (modulo persistence
  state) - refuse outright unless the user restates the exact scope and
  accepts the loss explicitly; then still show what the last RDB/AOF
  recovery point would restore.
- `DEBUG` subcommands (`DEBUG SLEEP`, `DEBUG SET-ACTIVE-EXPIRE`,
  `DEBUG JMAP`...) are undocumented operations - refuse and explain.
- `SCRIPT FLUSH` / `FUNCTION DELETE` drop all Lua/function state - same
  confirmation gate as `FLUSHDB` on a scripting-heavy workload.

---

## Replication health (read-only)

```bash
# Role, topology, lag - one command, grep the essentials
redis-cli --raw INFO replication | grep -E '^role|connected_slaves|slave[0-9]+|master_replid|master_repl_offset|slave_repl_offset|master_link_status|master_last_io_seconds_ago|master_sync_in_progress|slave_read_repl_offset|slave_read_only'
```

On a **primary** (`role:master`): `slave<N>` lines show each replica's
`ip,port,state,offset,lag`. Growing `lag` on one replica: inspect that
replica, not the primary. `master_repl_offset` stalled with clients waiting
on `WAIT` - check replica network, not replication config.

On a **replica** (`role:slave`): `master_link_status:down` with
`master_last_io_seconds_ago` growing means the link is dead - the replica
serves (possibly stale) data if `replica-serve-stale-data` is yes. Report
staleness; do not "fix" the link without diagnosis (`REPLICAOF` is a
topology write).

**Sentinel - observation only**:

```bash
# Sentinel runs on 26379 (or user-provided); SENTINEL masters is read-only
redis-cli -p 26379 --raw SENTINEL masters | head -40
redis-cli -p 26379 --raw SENTINEL replicas <master-name> | head -40
redis-cli -p 26379 --raw SENTINEL get-master-addr-by-name <master-name>
```

Never run `SENTINEL FAILOVER`, `SENTINEL RESET`, `SENTINEL REMOVE`, or
`SENTINEL MONITOR` - failover is a topology change with client-visible
impact, owned by the on-call operator through their own process. This
skill observes failover state (flags, `s_down`/`o_down`, epochs) and
reports it; it never triggers one.

`REPLICAOF` / `REPLICAOF NO ONE` / `CLUSTER *` are all topology writes:
list-before-change applies, and for cluster commands the skill defers
entirely (see scope rules above).

---

## Quick troubleshooting table

| Symptom | Locate with | Action |
|---------|-------------|--------|
| Latency spikes every N minutes | `latest_fork_usec` + `INFO persistence` timestamps | Fork cost from BGSAVE/BGREWRITEAOF; host-level fix is `ktuner` scope |
| Slow commands felt by clients | `SLOWLOG GET 10` + `INFO commandstats` | Identify the command and key; big-key split is a confirmed write |
| `evicted_keys` climbing | `INFO memory` + `INFO stats` hit/miss | Report policy semantics and sizing options; `CONFIG SET maxmemory` on confirmation |
| Clients blocked / timing out | `INFO clients` `blocked_clients` + `CLIENT LIST` cmd field | Classify BLPOP waiters vs stalls before acting |
| `rejected_connections` growing | `INFO clients` `maxclients` + OS ulimit (`ulimit -n`) | Raise is a host change (restart-class); report, don't tune |
| OOM command errors | `maxmemory` 0 or `volatile-*` policy with no TTL keys | Report policy/TTL mismatch; `CONFIG SET maxmemory-policy` on confirmation |
| Replica lag growing | `INFO replication` on primary and replica | Identify IO vs apply bottleneck; topology changes are confirmed writes |
| `master_link_status:down` | `INFO replication` on replica + `SENTINEL get-master-addr-by-name` | Report staleness and Sentinel view; no autonomous re-pointing |
| RDB save failing | `rdb_last_bgsave_status` + `CONFIG GET dir` + `df -h` | Disk space / permissions first; `SAVE` is refused, `BGSAVE` proposed |
| Writes suddenly all failing with `MISCONF` | `INFO persistence` `rdb_last_bgsave_status` | `stop-writes-on-bgsave-error` rejects writes after a failed BGSAVE; fix the save failure, never disable the guard to "restore" writes |
| Fragmentation ratio > 1.5 | `INFO memory` + stable-memory check | Report; `MEMORY PURGE` is a confirmed write; host swap (<1.0) is `ktuner` scope |
| Auth failure in step zero | `redis-cli PING` NOAUTH error | Ask the user for `REDISCLI_AUTH`; no guessing, no `-a` |

More read-only commands: `references/diagnostics-quickref.md`. Before any
flush, kill, config change, or save: `references/persistence-safety-checklist.md`.

---

## High-risk confirmation list

Show the user the impact and obtain explicit confirmation before any of:

1. `CLIENT KILL` (drops a live connection; `TYPE replica` desyncs
   replication)
2. `CONFIG SET` of any parameter (affects all clients immediately, lost on
   restart unless the config file is updated too)
3. `BGSAVE` / `BGREWRITEAOF` (fork + disk I/O cost; can stall the event
   loop on big datasets)
4. `MEMORY PURGE` (jemalloc defragmentation; server pauses briefly)
5. `UNLINK` / `DEL` of a big key (data deletion; `UNLINK` preferred for
   off-loop freeing)
6. `REPLICAOF` / `REPLICAOF NO ONE` (topology change; redirects or orphans
   data flow)
7. `SCRIPT FLUSH` / `FUNCTION DELETE` (drops all server-side code state)
8. Enabling `slowlog-log-slower-than` or `latency-monitor-threshold` on a
   busy production server (small but nonzero overhead)
9. `CLIENT PAUSE <timeout>` (freezes ALL client writes for the timeout;
   state the duration and that `CLIENT UNPAUSE` is the early release)
10. `MIGRATE` / `RESTORE` (moves or overwrites keys across instances;
    `MIGRATE` deletes the source key on success by default - state both
    ends)
11. `CONFIG REWRITE` (rewrites `redis.conf` in place; resolve the path via
    `INFO server` `config_file` first, confirm the file is writable, and
    warn that hand-written comments and manual edits may be lost)
12. `ACL SAVE` (persists current ACL state to the aclfile - same class of
    in-place config rewrite as `CONFIG REWRITE`)
13. `CONFIG RESETSTAT` / `SLOWLOG RESET` (destroys the diagnostic
    evidence the investigation stands on - connection counters, command
    stats, the slow log itself; confirm the user accepts losing the
    baseline before the reset, and capture the current values first)

Refuse outright, regardless of phrasing:

- `SHUTDOWN` / `SHUTDOWN NOSAVE` (availability drops to zero; `NOSAVE`
  also discards everything not yet persisted)
- Any ACL privilege write - `ACL SETUSER`, `ACL DELUSER`, `ACL LOAD`
  (privilege management; "grant myself access" after an auth failure is
  an escalation, not a fix)
- `SWAPDB` (silently swaps the contents of two DBs; rollback is manual
  and error-prone)
- `FAILOVER` (server-side primary takeover, Redis 6.2+ - a topology
  change, same rule as `SENTINEL FAILOVER`)
- `SAVE` on a production instance (synchronous full-dump block)
- Any `DEBUG` subcommand (undocumented, unsafe)
- `KEYS *` / `SMEMBERS` on suspect big keys / `MONITOR` (event-loop
  stalls, unbounded output)
- `SENTINEL FAILOVER` / `SENTINEL RESET` / `SENTINEL REMOVE` / any
  `CLUSTER` write subcommand (topology changes)

Refuse unless the user restates the scope and explicitly accepts the loss:

- `FLUSHALL` / `FLUSHDB` - even then, first show what the last RDB/AOF
  recovery point would restore

For each confirmation provide: the exact affected objects, how to roll back
(or that rollback is impossible), and a less destructive alternative. Until
the user agrees, run read-only commands only.

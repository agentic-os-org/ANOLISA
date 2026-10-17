# Redis diagnostics quick reference

Read-only commands, ordered global → object → detail. Safe on a
production instance as written, with two caveats: `--bigkeys` /
`--memkeys` traverse the whole keyspace (see Memory and keyspace), and
`MEMORY USAGE ... SAMPLES 0` is exact but O(N) on the event loop. All
invocations use `redis-cli --raw <command>` (bound with `head`/`grep`
where output can be large); never open an interactive shell and never
run `MONITOR`.

## Server identity and state

| Command | Purpose |
|---------|---------|
| `redis-cli --version` | Client version (7.x client matches 7.x server semantics) |
| `redis-cli --raw PING` | Liveness + auth probe; `NOAUTH` means ask the user, not guess |
| `redis-cli --raw INFO server \| grep -E 'redis_version\|redis_mode\|os:\|uptime_in_seconds'` | Version, mode (standalone/cluster/sentinel), uptime (stats observation window) |
| `redis-cli --raw INFO clients \| grep -E 'connected_clients\|blocked_clients\|maxclients'` | Client watermark and blocked count |
| `redis-cli --raw DBSIZE` | Key count of the selected DB (integer, O(1)) |
| `systemctl is-active redis redis-server valkey` | Liveness when redis-cli itself is the problem |

## Clients and connections

| Command | Purpose |
|---------|---------|
| `redis-cli --raw CLIENT LIST TYPE normal \| head -40` | Current client connections (6.2+ server-side filter excludes replica/pubsub; `head` is the bound on older servers) |
| `redis-cli --raw CLIENT LIST \| grep -c 'cmd='` | Connection count cross-check |
| `redis-cli --raw CLIENT LIST \| awk` idle filter (see SKILL.md) | Idle-heavy connections (>300s) |
| `redis-cli --raw INFO stats \| grep -E 'rejected_connections\|total_connections_received'` | Connection pressure and rejects since start |
| `redis-cli --raw INFO clients \| grep blocked_clients` | Blocked (BLPOP/XREAD/WAIT) client count |

## Latency and slow commands

| Command | Purpose |
|---------|---------|
| `redis-cli --raw CONFIG GET slowlog-log-slower-than slowlog-max-len` | Whether the slowlog is on and its capacity (0 = off) |
| `redis-cli --raw SLOWLOG GET 10` | Last 10 slow commands with full argument vectors |
| `redis-cli --raw SLOWLOG LEN` | Entry count (`SLOWLOG RESET` clears evidence - it is a write, confirm first, see checklist) |
| `redis-cli --raw CONFIG RESETSTAT` | Zeroes the `INFO stats`/`commandstats` counters - gated the same way: capture current values first, then only on explicit confirmation |
| `redis-cli --raw CONFIG GET latency-monitor-threshold` | Whether latency events are being recorded (0 = off) |
| `redis-cli --raw LATENCY LATEST` | Latest spike per event |
| `redis-cli --raw LATENCY HISTORY <event>` | Time series of one event (e.g. `event-loop`, `fork`, `expire`) |
| `redis-cli --raw INFO commandstats \| head -30` | Aggregate per-command cost - works even when the slowlog is off |
| `redis-cli --raw INFO stats \| grep latest_fork_usec` | Fork cost - the classic periodic-latency source |

## Memory and keyspace

| Command | Purpose |
|---------|---------|
| `redis-cli --raw INFO memory \| grep -E 'used_memory_human\|used_memory_rss_human\|maxmemory_human\|maxmemory_policy\|mem_fragmentation_ratio'` | The memory picture in one line-set |
| `redis-cli --raw INFO stats \| grep -E 'evicted_keys\|expired_keys\|keyspace_hits\|keyspace_misses'` | Eviction/expiration pressure and cache hit rate |
| `redis-cli --raw INFO keyspace` | Keys per DB with TTL counts (small output) |
| `redis-cli --bigkeys -i 0.01 \| tail -20` | SCAN-based sampler, biggest key per type - full keyspace traversal; minutes on large DBs, so throttle with `-i` and keep only the summary |
| `redis-cli --memkeys -i 0.01 \| tail -20` | Memory-focused sampler - full traversal plus one `MEMORY USAGE` per key; throttle it the same way |
| `redis-cli --raw SCAN <cursor> COUNT 1000` | Manual keyspace iteration - bound the loop (e.g. ~50 batches / ~30s), then report partial coverage and ask before continuing |
| `redis-cli --raw MEMORY USAGE <key>` | Per-key memory with default sampling (~5 elements per collection value) - cheap; `SAMPLES 0` is exact but O(N) on the event loop, single identified keys only |
| `redis-cli --raw MEMORY STATS \| head -30` | Allocator-level memory breakdown |
| `redis-cli --raw TYPE <key>` then `SCARD`/`HLEN`/`LLEN`/`XLEN` | Cardinality without materializing contents |

## Persistence

| Command | Purpose |
|---------|---------|
| `redis-cli --raw INFO persistence \| grep -E 'rdb_last_bgsave_status\|rdb_changes_since_last_save\|rdb_last_save_time\|aof_enabled\|aof_last_write_status\|aof_last_bgrewrite_status'` | RDB/AOF health in one view |
| `redis-cli --raw INFO persistence \| grep -E 'aof_current_size\|aof_last_write_size\|aof_delayed_fsync'` | AOF growth and fsync stalls |
| `redis-cli --raw INFO stats \| grep -E 'latest_fork_usec\|total_forks'` | Fork cost and frequency |
| `redis-cli --raw CONFIG GET dir dbfilename appendfilename appenddirname appendfsync` | Persistence file locations from the server, not a guessed path |
| `dir="$(redis-cli --raw CONFIG GET dir \| tail -1)"; case "$dir" in /*) df -h "$dir" ;; *) echo "CONFIG unavailable" ;; esac` | Free space on the persistence filesystem - validate before shell substitution (auth/permission errors leave stdout empty or an error string) |
| `redis-cli --raw CONFIG GET logfile` | Server log location for error follow-up |
| `redis-cli --raw LASTSAVE` | Epoch of the last successful RDB save |

## Replication and Sentinel (read-only)

| Command | Purpose |
|---------|---------|
| `redis-cli --raw INFO replication` | Role, connected replicas with offset/lag (on primary) or master link status (on replica) |
| `redis-cli --raw INFO replication \| grep -E '^role\|master_link_status\|master_last_io_seconds_ago'` | Quick replica-side link check |
| `redis-cli -p 26379 --raw SENTINEL masters \| head -40` | Sentinel's view of monitored masters (observation only) |
| `redis-cli -p 26379 --raw SENTINEL replicas <name> \| head -40` | Sentinel's view of replicas under a master |
| `redis-cli -p 26379 --raw SENTINEL get-master-addr-by-name <name>` | Current promoted master address per Sentinel |

## Config and ACL awareness (report, do not change)

| Command | Purpose |
|---------|---------|
| `redis-cli --raw CONFIG GET maxmemory maxmemory-policy` | Memory cap and eviction semantics |
| `redis-cli --raw CONFIG GET appendonly appendfsync auto-aof-rewrite-percentage` | Durability posture |
| `redis-cli --raw CONFIG GET save` | RDB snapshot schedule |
| `redis-cli --raw CONFIG GET replicaof replica-read-only` | Static replication config (INFO replication shows live state) |
| `redis-cli --raw ACL WHOAMI` | Current user (7.x) |
| `redis-cli --raw ACL GETUSER <current-user>` | Rules of the current user, to explain a `NOPERM` - mask `passwords`/`#<hash>` fields before reporting; never paste full `ACL LIST` output (it echoes unsalted SHA-256 password hashes, which are offline-crackable if the transcript leaks) |

All `CONFIG SET`, `CONFIG RESETSTAT`, `CLIENT KILL`, `SLOWLOG RESET`,
and persistence-triggering commands are writes - see
`references/persistence-safety-checklist.md` before any of them.

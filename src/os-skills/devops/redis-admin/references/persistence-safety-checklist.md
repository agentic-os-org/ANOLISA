# Persistence and safety checklist

Walk through this checklist before any write from the skill's high-risk
confirmation list - flush, kill, config change, save/rewrite trigger,
pause, migrate, counter reset, script/function state change, or
destructive key operation - on a Redis instance. If any item fails, stop
and return to read-only diagnostics.

## Before any data deletion (FLUSHDB / UNLINK / DEL of big keys)

- [ ] Scope stated in exact terms: which DB (`FLUSHDB` is the *current*
      selected DB, `FLUSHALL` is *every* DB - restate this to the user)
- [ ] Recovery point known: `LASTSAVE` timestamp and
      `rdb_changes_since_last_save` shown - the user knows how much data
      the last RDB would restore and how much would be lost
- [ ] AOF state known (`aof_enabled`): deleting keys is recorded in the
      AOF - the deletion itself propagates to replicas and persists
- [ ] Replicas of this server identified (`INFO replication` on the
      primary): a flush replays on every replica
- [ ] For big-key deletion: `UNLINK` proposed over `DEL` (asynchronous
      free, off the event loop); key name, type, and `MEMORY USAGE` shown
- [ ] `FLUSHALL` requires the user to restate the scope and accept the
      loss explicitly - a generic "go ahead" is not sufficient

## Before any CONFIG SET

- [ ] Current value captured first (`CONFIG GET <param>`) - the rollback
      is `CONFIG SET <param> <old-value>`. Exception: credential
      parameters (`requirepass`, `masterauth`, `masteruser`,
      `tls-key-file-pass`, `tls-client-key-file-pass`) are never
      captured and never set - reading the live value back would leak it
      into the transcript, and setting it is refused outright (see the
      refusal list)
- [ ] Restart persistence noted: CONFIG SET does not survive a restart
      unless the config file is also updated - who edits the file, and when
- [ ] If the change must survive restart: `CONFIG REWRITE` is itself a
      confirmed write (rewrites `redis.conf` in place, may drop
      hand-written comments and manual edits) - resolve the file via
      `INFO server` `config_file` first and warn about comment loss;
      `ACL SAVE` is the same class of in-place rewrite for the aclfile
- [ ] Value sanity-checked against the workload (e.g. raising
      `maxmemory` beyond `total_system_memory` invites the OOM killer;
      `maxmemory-policy` changes eviction semantics for all keys)
- [ ] `appendonly` toggles (RDB↔AOF switches) are restart-class changes -
      they re-write the dataset's persistence story; never flip casually

## Before triggering BGSAVE / BGREWRITEAOF

- [ ] `latest_fork_usec` checked - a fork cost near or above the
      latency budget means the event loop will stall; state the expected
      stall from the last observed fork
- [ ] Disk space under `CONFIG GET dir` checked (`df -h`) against
      dataset size (RDB) or current AOF size (rewrite can transiently
      need up to 2x: old + new file)
- [ ] `rdb_last_bgsave_status` / `aof_last_bgrewrite_status` from the
      last run shown - a previous failure has a cause to fix first
      (disk full, permissions), or the new run fails the same way
- [ ] Not during peak: replication in progress (`master_repl_offset`
      moving fast) or a heavy command window raises combined I/O cost
- [ ] `SAVE` (synchronous) is refused on production; only `BGSAVE` is
      ever proposed

## Before CLIENT KILL

- [ ] Target client shown: id, addr, age, idle, last command
      (`CLIENT LIST` line)
- [ ] Client is not `flags=S` / a replication connection - killing it
      desyncs a replica; that is a topology change, not a cleanup
- [ ] Rollback impossible: the application owning the connection must
      reconnect - confirm the application tolerates it

## Before CLIENT PAUSE / MIGRATE / RESTORE / FUNCTION RESTORE

- [ ] `CLIENT PAUSE <timeout>`: duration stated in seconds and the write
      freeze it implies (ALL client writes, not just one client);
      `CLIENT UNPAUSE` named as the early release; business tolerance
      for the full timeout confirmed
- [ ] `MIGRATE <key> <target-host>`: target instance reachable and its
      auth method known; default semantics delete the source key on
      success - use `COPY` if the source must survive, and state that
      there is no rollback once the source is gone
- [ ] `RESTORE <key> ... <payload>`: the key's current state captured
      first (`EXISTS` / `TYPE` / `MEMORY USAGE`) - RESTORE overwrites a
      same-named key, and the overwrite is not undoable;
      `FUNCTION RESTORE` follows the same rule at library scope - the
      `FLUSH` and `REPLACE` policies delete or overwrite the existing
      libraries, so capture the forensics first, split by purpose:
      `FUNCTION LIST WITHCODE` for the source the user reads, and
      `FUNCTION DUMP` for the recoverable copy (serialized binary -
      redirect it to a file per "During execution", never into the
      conversation)

## Before MEMORY PURGE

- [ ] Allocator confirmed as jemalloc (`INFO memory` `mem_allocator`) -
      on other allocators the purge is a no-op at best
- [ ] Fragmentation actually present (`mem_fragmentation_ratio` > ~1.5
      with stable `used_memory`) and not a fork (COW) in progress - a
      transient ratio spike is not defragmentation debt
- [ ] Brief main-thread stall accepted: the purge briefly blocks
      allocation on the server - state it, and prefer reporting over
      running it

## Before SCRIPT FLUSH / FUNCTION DELETE / FUNCTION FLUSH

- [ ] Server-side code inventory captured first so the user sees
      exactly what will be dropped: `FUNCTION LIST` for function
      libraries (`FUNCTION DUMP` adds a recoverable copy); the Lua
      script cache cannot be enumerated - take the SHA inventory from
      the app or deployment side and confirm each digest with
      `SCRIPT EXISTS <sha> ...`; without an inventory the state is not
      safely reloadable, and after `SCRIPT FLUSH` the lost scope can
      no longer be verified
- [ ] Reload path known: who reloads the scripts/libraries and from
      where (the app deploy pipeline? a repo of `SCRIPT LOAD` /
      `FUNCTION LOAD` calls?) - if nobody can answer, the state is not
      safely reloadable
- [ ] For functions specifically: `FUNCTION DELETE` / `FUNCTION FLUSH`
      replicate to replicas - the drop replays everywhere in the topology

## Before EVAL / SCRIPT LOAD / FUNCTION LOAD

- [ ] Full script/function body shown to the user as part of the
      confirmation - a digest alone is never enough (obtain the
      `EVALSHA` source first or refuse, see the Never list; for a
      loaded library the readable source is `FUNCTION LIST WITHCODE`
      - plain `FUNCTION LIST` lists functions without their code)
- [ ] Code provenance known: who wrote it, from which repo or deploy
      pipeline it comes - the entry side of the reload-path question
      above
- [ ] `FUNCTION LOAD ... REPLACE` overwrites a same-named existing
      library - capture the forensics first, the same split-purpose
      rule `RESTORE` requires of the key it overwrites: `FUNCTION
      LIST WITHCODE` for the human-readable source, `FUNCTION DUMP`
      for the recoverable copy (binary - redirect to a file, see
      "During execution")
- [ ] Abort path stated in the confirmation: `SCRIPT KILL` (or
      `FUNCTION KILL` for a running function) ends a script that is
      still running, and stays available while the server is busy
      inside it - but only before the script has written a key. Once
      it has written, Redis refuses the kill and the only remaining
      option is `SHUTDOWN NOSAVE`, refused outright in the Never
      list - so any body containing writes must say it cannot be
      aborted once it starts writing

## Before CONFIG RESETSTAT / SLOWLOG RESET

- [ ] Current values captured first (`INFO commandstats`, `INFO stats`,
      `SLOWLOG LEN`) - after the reset they are gone
- [ ] User confirmed they accept losing the diagnostic baseline (these
      counters are usually the evidence an ongoing investigation stands
      on)

## Before topology actions (REPLICAOF / SENTINEL *)

- [ ] `REPLICAOF` / `REPLICAOF NO ONE`: current role, target master, and
      the data-flow consequence stated (replica discards its own data to
      sync from the new master)
- [ ] `SENTINEL FAILOVER` / `RESET` / `REMOVE` / `MONITOR` / `SET`:
      this skill observes and reports only - hand these to the on-call
      operator's own process, with the observed state as input
- [ ] Cluster (`CLUSTER *`) write subcommands are out of scope entirely
      - report and defer

## During execution

- [ ] Output redirected to a log file for long operations, not streamed
      into the conversation
- [ ] One operation at a time; no parallel saves/rewrites competing for
      disk and fork
- [ ] On unexpected error (`MISCONF` write rejections after a failed
      BGSAVE, disk full mid-rewrite, `LOADING` during restart, `READONLY`
      on a replica, `OOM` at maxmemory, `NOPERM` from ACLs): stop and
      report; never retry blind

## After execution

- [ ] `INFO persistence` re-checked (`rdb_last_bgsave_status`,
      `aof_last_write_status` now clean)
- [ ] `INFO replication` re-checked if replication was involved
- [ ] Disk space re-checked (AOF rewrite leaves `.old` artifacts
      transiently)
- [ ] Result reported with the new recovery point (LASTSAVE / AOF size),
      not just "done"

## Never (refuse outright)

- `SHUTDOWN` / `SHUTDOWN NOSAVE` - availability drops to zero; `NOSAVE`
  also discards everything not yet persisted
- Any ACL privilege write - `ACL SETUSER`, `ACL DELUSER`, `ACL LOAD`.
  Privilege management is never a diagnostic shortcut: "grant myself
  access" after an auth failure is escalation, not a fix (`ACL SAVE` is
  confirm-gated, like `CONFIG REWRITE`)
- `SWAPDB` - silently swaps the contents of two DBs; rollback is manual
  and error-prone
- `FAILOVER` (Redis 6.2+) - server-side primary takeover; same rule as
  `SENTINEL FAILOVER` / `SENTINEL RESET` / `SENTINEL REMOVE` /
  `SENTINEL MONITOR` / `SENTINEL SET` (topology changes, including
  registering a new monitor; `SENTINEL SET` writes `auth-pass` /
  `auth-user` credentials - same rule as the `CONFIG SET` credential
  refusal - retunes the failover triggers `quorum` /
  `down-after-milliseconds`, and immediately rewrites `sentinel.conf`)
- `KEYS *` on any nontrivial production instance - O(N) event-loop block;
  use `SCAN` with cursors
- `SMEMBERS` / `HGETALL` / `LRANGE 0 -1` on keys identified or suspected
  as big - bound with `SCARD`/`HLEN`/`LLEN` first, then `*SCAN ... COUNT`
- `MONITOR` - streams every command (perf + potentially secrets in args)
- `SAVE` on production - synchronous full-dump block
- Any `DEBUG` subcommand - undocumented, unsafe (`DEBUG SLEEP` stalls the
  server, `DEBUG SET-ACTIVE-EXPIRE` changes expiry semantics)
- Passing `-a <password>` on the redis-cli command line - visible in
  process lists and shell history; use `REDISCLI_AUTH` supplied by the
  user
- `FLUSHALL` / `FLUSHDB` outside the confirmation gate above - an
  ambiguous "clean it up" is not a scope statement, and a generic "go
  ahead" is not loss acceptance
- `CONFIG SET` on a parameter whose current value was not captured first
  (exception: credential parameters - `requirepass`, `masterauth`,
  `masteruser`, TLS key passwords - are refused outright and never
  captured, see "Before any CONFIG SET")
- `EVAL` / `EVALSHA` / `FCALL` whose script body invokes any command
  this skill refuses outright or refuses unless the scope is restated -
  judge a script by what it calls (`redis.call('FLUSHALL')` is
  `FLUSHALL`), not by the verb; `EVAL_RO` / `EVALSHA_RO` / `FCALL_RO`
  are exempt from the write side of that scrutiny only: the server
  rejects write commands inside them but not expensive read-only
  ones, so `redis.call('KEYS', '*')`, `SMEMBERS` / `HGETALL` /
  `LRANGE` on a suspect big key, or a `MEMORY USAGE ... SAMPLES 0`
  loop in an `_ro` form inherits the same refusal as the bare
  command. Digest-only `EVALSHA` / `EVALSHA_RO` carry no recoverable
  body - obtain the source from the user or app side and judge it
  first, refusing if it cannot be produced (`EVALSHA_RO` alone may
  run unreviewed only when the caller can state what the body does -
  which key it reads, what it returns - and only because the server
  rejects write commands inside it: read-only is not cheap). `SCRIPT
  LOAD` / `FUNCTION LOAD` (installing server-side code) go through
  the confirmation gate above, never silently

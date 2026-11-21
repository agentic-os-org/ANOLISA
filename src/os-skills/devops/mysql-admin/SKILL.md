---
name: mysql-admin
version: 0.1.0
description: Use this skill when operating a running MySQL (or MariaDB) instance - diagnosing processlist and lock waits, investigating deadlocks and connection exhaustion, auditing slow queries and redundant indexes, governing backup and restore with mysqldump and binlog positions, and checking replication health, tablespace growth, and key server variables. Triggers on mysql client sessions, SHOW PROCESSLIST, performance_schema lock analysis, EXPLAIN and slow query log work, mysqldump --single-transaction, binlog/point-in-time recovery, and SHOW REPLICA STATUS checks.
layer: application
lifecycle: production
category: devtools
tags: [mysql, mariadb, database, dba, replication, slow-query, backup, troubleshooting]
platforms:
  - claude-code
  - gemini-cli
  - openai-codex
license: Apache-2.0
maintainers:
  - github: Zzzzzmeow
---

# MySQL Instance Administration

Operation of a running MySQL 8.0 instance (with MariaDB equivalents where they
apply). This skill covers the safety-critical day-to-day work of a database
operator: diagnosing what the server is doing right now, finding blocking
locks and deadlock causes, auditing slow queries and index health, governing
backups so restores stay possible, and verifying replication and capacity.
The instance is treated as a production asset: read-only inspection comes
first, every write or terminate action is listed and confirmed before it
runs, and no destructive command is issued as a "quick fix".

---

## When to use this skill

Trigger this skill when the user:

- Asks what a MySQL server is doing: processlist, running queries, lock waits
- Reports lock wait timeouts (SQLSTATE `HY000` 1205) or deadlocks (1213)
- Suspects connection exhaustion ("too many connections") and wants a watermark check
- Wants to find and analyze slow queries, or audit missing/redundant/unused indexes
- Asks to run EXPLAIN on a query and interpret the plan
- Needs a backup with `mysqldump`, wants to know the binlog position, or needs to restore
- Wants to verify replica health, replication lag, or tablespace growth
- Asks whether key server variables (buffer pool, connection caps) are sized sensibly

Do NOT trigger this skill for:

- Installing, upgrading, or initializing a MySQL server - deployment topic,
  covered by distribution package management, not this skill
- MySQL server parameter tuning (`innodb_buffer_pool_size` redesign,
  `my.cnf` rework) - out of scope for this skill: it only reports current
  values and leaves the decision to the user. Host-level kernel and memory
  parameters are the `ktuner` skill's domain; `my.cnf`/InnoDB sizing is
  neither this skill's nor `ktuner`'s to change
- Middleware and topology decisions (ProxySQL routing, InnoDB Cluster /
  Group Replication topology changes, sharding) - a change-management topic
- Managed cloud RDS instances - vendor consoles and APIs expose these
  diagnostics differently; do not assume `SUPER`-class privileges exist there
- General host-level backup workflows (filesystem snapshots, LVM, cloud disk
  snapshots) - the `backup-restore` skill owns those layers and also lists
  basic `mysqldump` recipes; when the task is specifically a MySQL dump or
  restore, this skill's rules take precedence (consistency semantics,
  binlog positions, confirmation gates), and `backup-restore`'s MySQL
  sections point here for those
- Application-side ORM or SQL query rewriting - this skill explains plans,
  it does not rewrite application code

---

## Key principles

1. **Read-only first** - Diagnose with `SHOW` statements,
   `information_schema`, `performance_schema`, and `sys` views before
   touching anything. The safe default for every investigation is a
   read-only query; writes require an explicit reason.
2. **List before killing or changing** - Before `KILL`, `DROP`, `TRUNCATE`,
   `ALTER`, or any `SET GLOBAL`, list the exact affected objects (query IDs,
   table names, current vs proposed value) and get user confirmation.
3. **No interactive sessions** - Agent runtimes are non-interactive. Never
   open a bare `mysql` shell; run every statement as one bounded batch
   invocation: `mysql --batch --raw -e "SELECT ..."`. Never use `pager` or
   other blocking client commands.
4. **Never guess credentials or paths** - Connect with whatever the
   environment provides (socket + current user, `~/.my.cnf`, or a
   user-supplied defaults file). If authentication fails, ask the user how
   they authenticate; do not try password combinations and never put a
   password on the command line where `ps` can see it. Locate datadir, logs,
   and config with `SELECT @@datadir` / `@@slow_query_log_file` etc., not by
   assuming `/var/lib/mysql`.
5. **MySQL 8.0 semantics, MariaDB mapped** - The commands below assume
   MySQL 8.0: replicas use `SHOW REPLICA STATUS` (8.0.22+;
   `SHOW SLAVE STATUS` works on ALL of 8.0, deprecated since 8.0.22 and
   removed in 8.4), `EXPLAIN ANALYZE` exists, and
   `caching_sha2_password` is the default auth plugin. MariaDB (what
   `dnf install mysql` actually installs on RHEL/Anolis-family
   distributions) lacks several sources used below - substitute the
   equivalents in the mapping table in step zero rather than assuming
   identical output.
6. **Bounded output** - `SHOW ENGINE INNODB STATUS` and
   `information_schema` tables can be huge. Pipe through `head`, add
   `ORDER BY ... LIMIT`, or redirect to a temp file for analysis; never dump
   a full table scan of `performance_schema.events_statements_summary_by_digest`
   into the conversation.

---

## Step zero: probe the environment

Run these read-only checks before any other action:

```bash
# Client availability and version
command -v mysql mysqladmin mysqldump
mysql --version

# Connectivity and auth - try the zero-config path first (local socket +
# current user, or ~/.my.cnf). A failure here is a question to the user,
# not a password guessing exercise.
mysql --batch --raw -e "SELECT VERSION(), @@version_comment;"

# If that fails, check whether a local server exists before asking anything
command -v mysqld systemctl 2>/dev/null
systemctl is-active mysqld mysql mariadb 2>/dev/null

# Server facts that shape every later decision (read via SQL, not config paths)
mysql --batch --raw -e "
SELECT @@version, @@version_comment, @@hostname, @@port, @@socket;
SELECT @@datadir, @@tmpdir;
SELECT @@read_only, @@super_read_only;
"
```

If the user provides a defaults file, use it explicitly:
`mysql --defaults-extra-file=/path/extra.cnf --batch --raw -e "..."`.
`@@read_only` matters immediately: on a read-only primary or a replica,
write attempts fail by design - report it instead of working around it with
`SUPER`-style escalation.

**Privilege probe** - several core diagnostics silently degrade without the
right privileges; probe before concluding anything:

```bash
# What this account can actually do
mysql --batch --raw -e "SHOW GRANTS;"

# PROCESS privilege gate (ERROR 1227 = no PROCESS): without it,
# information_schema.innodb_trx is unreadable and processlist shows only
# this session's own threads - "no long queries" would be a false negative
mysql --batch --raw -e "SELECT COUNT(*) FROM information_schema.innodb_trx;"
```

Without `PROCESS`: lock-wait analysis and the full processlist are
unavailable - report the gap instead of diagnosing from partial data.
The capacity tables `information_schema.files` and
`innodb_tablespaces` also require `PROCESS` (FILES since 8.0.21) -
without it, disk analysis falls back to `df -h` on `@@datadir` plus `du`.
Without `REPLICATION CLIENT`: `SHOW REPLICA STATUS` returns an empty set -
that means "cannot see replication", not "replication is not configured".
`SHOW REPLICAS` (listing the replicas attached to this source) needs
`REPLICATION SLAVE` instead. Treat anomalously empty results as a
permission question first - with one exception: a replication-status grep
that returns nothing may also be a field-name version difference
(`Replica_*` vs `Slave_*`), not a permission gap.

**Processlist source gate** - `performance_schema.processlist` (the
preferred source below) requires MySQL 8.0.22+ with
`performance_schema=ON`; MariaDB has no such table. Probe once:

```bash
mysql --batch --raw -e "SELECT @@version, @@performance_schema;"
mysql --batch --raw -e "SHOW TABLES FROM performance_schema LIKE 'processlist';"
# P_S processlist COUNTS can silently undercount when
# performance_schema_max_thread_instances < max_connections (a common
# memory-saving setting) - check the two together
mysql --batch --raw -e "SELECT @@performance_schema_max_thread_instances, @@max_connections;"
```

| Processlist source | Available on | Trade-off |
|--------------------|--------------|-----------|
| `performance_schema.processlist` | MySQL 8.0.22+, `performance_schema=ON` | Preferred: no global mutex. `ERROR 1146` = pre-8.0.22 or MariaDB; P_S off = silently partial data |
| `information_schema.processlist` | All versions | Deprecated in 8.0; takes a global mutex on busy servers |
| `SHOW PROCESSLIST` | All versions | Same implementation as the I_S table by default; `performance_schema_show_processlist=ON` (8.0.22+) switches it to the lockless P_S implementation |

**MariaDB equivalents** for sources used in this skill:

| MySQL 8.0 form | MariaDB equivalent |
|----------------|--------------------|
| `performance_schema.processlist` | `SHOW FULL PROCESSLIST` / `information_schema.processlist` |
| `performance_schema.data_locks` / `data_lock_waits` | `information_schema.innodb_locks` / `innodb_lock_waits` |
| `SHOW REPLICA STATUS` (and its `Replica_*` field names) | `SHOW SLAVE STATUS` / `SHOW ALL SLAVES STATUS` (fields are `Slave_*` / `Seconds_Behind_Master`) |
| `performance_schema.replication_connection_status` / `replication_applier_status_by_worker` | `SHOW SLAVE STATUS` |
| `sys.schema_redundant_indexes` / `schema_unused_indexes` | not shipped - probe `SHOW TABLES FROM sys LIKE 'schema_%indexes';`, report the gap if absent |
| `information_schema.innodb_tablespaces` | `INNODB_SYS_TABLESPACES`, but WITHOUT `FILE_SIZE`/`ALLOCATED_SIZE` columns - use `df`/`du` on `@@datadir` instead |
| `@@gtid_mode` | no such variable - check `gtid_strict_mode` / `gtid_current_pos` |
| `performance_schema.events_statements_summary_by_digest` | table exists, but MariaDB ships `performance_schema=OFF` by default - enablement is a user decision |
| mysqldump `--source-data` / `--set-gtid-purged` | `--master-data` / `--gtid` |

---

## Runtime diagnostics: processlist, locks, deadlocks, connections

```bash
# What is running right now (bounded, unformatted listing of long runners)
mysql --batch --raw -e "SHOW PROCESSLIST;" | head -40

# Long-running queries only (> 60s), oldest first. Prefer
# performance_schema.processlist (no global mutex - probe it in step
# zero); where unavailable (pre-8.0.22, P_S off, MariaDB) fall back to
# information_schema.processlist and note it is deprecated and takes a
# global mutex on busy servers - exactly when this query is needed
mysql --batch --raw -e "
SELECT id, user, host, db, TIME AS seconds, state, LEFT(info, 120) AS query
FROM performance_schema.processlist
WHERE command <> 'Sleep' AND TIME > 60
ORDER BY TIME DESC LIMIT 20;"

# Connection picture: current load plus the high-water mark. Count via
# Threads_connected (no privilege, no P_S dependency, works on all
# versions - a P_S processlist COUNT(*) can silently undercount when
# max_thread_instances is below max_connections, see the source gate).
# P_S/I_S processlist remains the source for DETAIL rows, not counts.
# (Max_used_connections_time answers "when did we hit the peak")
mysql --batch --raw -e "
SELECT @@max_connections AS cap,
       @@thread_cache_size AS thread_cache;"
mysql --batch --raw -e "SHOW GLOBAL STATUS LIKE 'Threads_connected';"
mysql --batch --raw -e "SHOW GLOBAL STATUS LIKE 'Max_used_connections%';"
```

**Lock waits** (who blocks whom - the authoritative source is
`performance_schema`, not guessing from processlist states):

```sql
SELECT r.trx_mysql_thread_id AS waiting_thread, r.trx_query AS waiting_query,
       b.trx_mysql_thread_id AS blocking_thread, b.trx_query AS blocking_query,
       r.trx_wait_started
FROM performance_schema.data_lock_waits w
JOIN information_schema.innodb_trx r ON r.trx_id = w.REQUESTING_ENGINE_TRANSACTION_ID
JOIN information_schema.innodb_trx b ON b.trx_id = w.BLOCKING_ENGINE_TRANSACTION_ID;
```

**Deadlocks**: the last recorded deadlock is inside
`SHOW ENGINE INNODB STATUS` (section `LATEST DETECTED DEADLOCK`) - extract
only that section; the full output is tens of KB:

```bash
mysql --batch --raw -e "SHOW ENGINE INNODB STATUS\G" \
  | sed -n '/^LATEST DETECTED DEADLOCK$/,/^TRANSACTIONS$/p' | head -120
# Only the conclusion line (which transaction was rolled back):
mysql --batch --raw -e "SHOW ENGINE INNODB STATUS\G" \
  | grep -E '^\*\*\* WE ROLL BACK'
```

Note that
`innodb_print_all_deadlocks=ON` is required to keep history in the error log;
if it is `OFF`, only the latest deadlock is available. Deadlock analysis is
read-only by nature: report the two transactions and their lock order, and
let the user decide the application-side fix (consistent access order,
shorter transactions, or index changes to reduce gap locks).

**Killing a query is a write action**: `KILL <id>` (or `KILL QUERY <id>` to
cancel only the current statement, keeping the connection). Follow the
list-before-change rule: show the query text, its runtime, and what a
rollback will discard, then act on confirmation.

---

## Slow query and index audit

Locate the slow log from the server, not from a guessed path:

```bash
mysql --batch --raw -e "
SELECT @@slow_query_log, @@slow_query_log_file, @@long_query_time,
       @@log_output;"
# Rotate-analysis tooling availability
command -v mysqldumpslow pt-query-digest 2>/dev/null
```

If `@@slow_query_log` is `OFF`, do not flip it silently - enabling it is a
server change (propose `SET GLOBAL slow_query_log = ON;` with its cost and
let the user decide). Prefer read-only alternatives while it is off: the
statement digest summary.

```sql
-- Top offenders by total time (bounded)
SELECT SCHEMA_NAME, DIGEST_TEXT AS query_pattern,
       COUNT_STAR AS exec_count, SUM_TIMER_WAIT/1e12 AS total_s,
       AVG_TIMER_WAIT/1e9 AS avg_ms, SUM_ROWS_EXAMINED
FROM performance_schema.events_statements_summary_by_digest
ORDER BY SUM_TIMER_WAIT DESC LIMIT 10;
```

**EXPLAIN**: always `EXPLAIN` first (it never executes). `EXPLAIN ANALYZE`
(MySQL 8.0.18+) really executes the statement - it is NOT read-only:

```sql
EXPLAIN FORMAT=TREE SELECT ...;   -- plan shape, no execution
EXPLAIN ANALYZE     SELECT ...;   -- executes the statement for real
```

- Use `EXPLAIN ANALYZE` only on plain `SELECT`s. MySQL 8.0 also supports
  it for multi-table `UPDATE`/`DELETE` - and it really modifies data -
  so running it on any write statement is forbidden outright, not a
  confirmation-gated action.
- `SELECT ... FOR UPDATE` / `FOR SHARE` under `EXPLAIN ANALYZE` take real
  row locks - do not run them.
- It executes the whole statement with no implicit LIMIT - on a large
  table that is a full read pushed onto the primary. Ask the user first
  on busy servers, or narrow the WHERE / add LIMIT before running.

Read the plan for: full table scans on large tables (`type=ALL`), rows
examined vs returned ratio, filesort/temporary tables, and index choices
(`possible_keys` vs `key`). Recommendations go to the user as proposals;
`ALTER TABLE ... ADD INDEX` is a write operation with lock/online-DDL
considerations - list the DDL, its ALGORITHM/LOCK implications, and wait for
confirmation.

**Index health** (all read-only):

```sql
-- Redundant indexes (MySQL 8.0 ships the sys schema; bounded - the full
-- view is one wide row per column pair, SELECT * floods batch output)
SELECT table_schema, table_name, redundant_index_name, dominant_index_name
FROM sys.schema_redundant_indexes LIMIT 20;

-- Unused indexes since server restart (stats reset on restart - caveat it)
SELECT object_schema, object_name, index_name
FROM sys.schema_unused_indexes
WHERE object_schema NOT IN ('mysql','sys','performance_schema') LIMIT 20;

-- Table sizes driving index decisions
SELECT table_schema, table_name,
       ROUND((data_length+index_length)/1024/1024) AS total_mb,
       ROUND(index_length/1024/1024) AS index_mb, table_rows
FROM information_schema.tables
WHERE table_schema NOT IN ('mysql','sys','performance_schema','information_schema')
ORDER BY (data_length+index_length) DESC LIMIT 15;
```

Dropping an "unused" or "redundant" index is a write action gated on the
uptime caveat above: an index unused since last restart may serve monthly
reporting. Always state the observation window.

---

## Backup and restore governance

The `backup-restore` skill owns the general backup policy (workspace, user
data, OS, cloud snapshots). This section governs the MySQL-specific
mechanics: consistency, binlog positions, and disk safety.

```bash
# Preconditions - all read-only (binlog_format and gtid_mode via SHOW
# VARIABLES where noted: the @@variable form errors on 8.4+ /
# deprecated since 8.0.34, and @@gtid_mode does not exist on MariaDB)
mysql --batch --raw -e "
SELECT @@log_bin, @@server_id;
SHOW VARIABLES LIKE 'binlog_format';
SELECT @@datadir;"
mysql --batch --raw -e "SELECT @@gtid_mode;"   # MariaDB: ERROR 1193 - see mapping table

# Long write transactions in flight? --source-data waits (via FTWRL) for
# them to finish - check before dumping, not after the server stalls
# (needs the PROCESS privilege; an ERROR 1227 here means report the
# permission gap, do not conclude "no transactions")
mysql --batch --raw -e "
SELECT trx_mysql_thread_id, trx_started,
       TIMESTAMPDIFF(SECOND, trx_started, NOW()) AS age_s, LEFT(trx_query, 120) AS q
FROM information_schema.innodb_trx
WHERE trx_started < NOW() - INTERVAL 5 SECOND
ORDER BY trx_started LIMIT 10;"

# Disk space check BEFORE dumping: size the dump against available space
mysql --batch --raw -e "
SELECT table_schema,
       ROUND(SUM(data_length+index_length)/1024/1024/1024, 1) AS gb
FROM information_schema.tables
GROUP BY table_schema ORDER BY SUM(data_length+index_length) DESC;"
df -h "$(mysql --batch --raw -N -e 'SELECT @@datadir;')"   # free space where data lives
df -h .                                                    # free space where the dump lands
```

**Logical backup** (InnoDB; `--single-transaction` gives the lock-free
consistent snapshot - the dump options below decide whether it stays
lock-free):

```bash
# GTID server (gtid_mode=ON), MySQL 8.0.32+, restore target is a fresh or
# empty-gtid_executed instance (seeding): no FTWRL at all
mysqldump --single-transaction --set-gtid-purged=ON \
  --routines --triggers --events <db> > dump_$(date +%F).sql

# Non-GTID server, or GTID server with an existing restore target:
# takes the short FTWRL window described below
mysqldump --single-transaction --source-data=2 --set-gtid-purged=OFF \
  --routines --triggers --events <db> > dump_$(date +%F).sql
```

- `--single-transaction` alone gives a consistent snapshot without table
  locks (InnoDB only; MyISAM tables are NOT covered by its consistency).
- **Combined with `--source-data=N`, mysqldump executes
  `FLUSH TABLES WITH READ LOCK` at the start of the dump**: it reads the
  binlog position, then releases the lock. FTWRL queues ALL writes behind
  it, and it waits for in-flight long write transactions to finish first -
  the whole-server write stall window. This is why the preflight above
  checks for them, and why the dump runs off-peak.
- `--source-data=2` (`--master-data=2` on MariaDB and MySQL < 8.0.26 -
  the two options are identical in effect, the boundary is the client
  version) records the binlog file and position as a comment in the dump -
  the anchor for point-in-time recovery. Never omit it on a binlog-enabled
  non-GTID server. **Exception**: on a GTID server seeding via
  `--set-gtid-purged=ON`, the anchor is the dump's
  `SET @@GLOBAL.gtid_purged` set and `--source-data` is deliberately
  omitted - adding it back would re-introduce the FTWRL window on the one
  command form that exists to avoid it.
- **`--set-gtid-purged=ON` is the FTWRL-free GTID form, gated on three
  conditions**: (a) MySQL 8.0.32+ - before that, combining it with
  `--single-transaction` could produce inconsistent dump output per the
  manual, so on 8.0.0-8.0.31 either accept the short FTWRL window of
  `--source-data=2` or use `--set-gtid-purged=ON --lock-all-tables`:
  **`--lock-all-tables` holds a global read lock for the ENTIRE dump
  (hours on a large database, not a short window) and automatically turns
  OFF `--single-transaction`** - consistency then depends on that lock
  alone, so drop `--single-transaction` from the command and tell the
  user the full write-stop duration. It is a heavier option than the
  `--source-data=2` FTWRL window, justified only when
  `--set-gtid-purged=ON` is required and upgrading past 8.0.31 is not
  possible; (b) GTID enabled on the source (`gtid_mode=ON`, plus the
  `RELOAD` or `FLUSH_TABLES` privilege); (c) a restore target that is
  fresh or has an empty `gtid_executed` (seeding) - replaying it into an
  instance that already holds the same GTIDs fails on the embedded
  `SET @@GLOBAL.gtid_purged`, and the tempting fix (`RESET MASTER` - see
  the Never list) destroys the recovery chain. For an existing GTID
  target use `OFF` with `--source-data=2` and accept the short FTWRL
  window; `COMMENTED` (8.0.17+) records the GTID information as comments
  when it must be kept but not auto-applied - note that only
  `SET @@GLOBAL.gtid_purged` is commented out;
  `SET @@SESSION.sql_log_bin=0` is still active in the dump.
- **`--set-gtid-purged` also decides whether the restore propagates**:
  `ON`, `COMMENTED`, and `AUTO` on a GTID-enabled source embed
  `SET @@SESSION.sql_log_bin=0` in the dump - the restore does NOT enter
  the binlog and does NOT reach any replica. With `OFF` the restore
  replays into the binlog and propagates to every replica. State which
  case applies before restoring on a primary with attached replicas.
- MariaDB's mysqldump has no `--set-gtid-purged`; the equivalent is
  `--gtid`.
- `--routines --triggers --events` default-wise matter: plain `mysqldump <db>`
  includes triggers but NOT routines/events - state what is excluded.

**Restore is the highest-risk operation in this skill.** Walk
`references/backup-safety-checklist.md` first. Core rules:

- Restore into an explicitly named target database, never blind into an
  existing production schema; verify the dump's target
  (`grep -n -m5 -E '^(CREATE DATABASE|USE|DROP TABLE)' dump.sql` - `head`
  is not reliable on dumps with routines/events).
- Check disk space against the dump size times a load factor (InnoDB growth,
  index rebuild during `INSERT`).
- Keep the restore anchor of the restore point; `RESET MASTER` (the only
  spelling on MySQL 8.0, deprecated since 8.0.35; renamed `RESET BINARY
  LOGS AND GTIDS` in 8.2/8.4, old spelling removed in 8.4; MariaDB also
  `RESET MASTER`) on a primary destroys the recovery chain - only ever on
  explicit user instruction after showing what is lost.
- Know the dump's `--set-gtid-purged` form before restoring on a primary
  with attached replicas: an `ON`/`AUTO`/`COMMENTED` dump restores with
  `sql_log_bin=0` and does NOT propagate; an `OFF` dump replays into the
  binlog and reaches every replica (see the backup section above).

---

## Instance health: replication, tablespaces, key variables

```bash
# Replication health. The grep matches BOTH field-name generations:
# Replica_* (8.0.22+) and Slave_* (earlier 8.0, MariaDB) - a filter that
# only knows one name returns empty on the other and looks like a
# permission problem
mysql --batch --raw -e "SHOW REPLICA STATUS\G" | grep -Ei '(Replica|Slave)_(IO|SQL)_Running|Seconds_Behind|Last_.*Error'

# This instance AS A REPLICA: its I/O thread's connection to the upstream
# source (empty on a pure source - that is not an error)
mysql --batch --raw -e "SELECT * FROM performance_schema.replication_connection_status;"

# Attached replicas of THIS source (needs REPLICATION SLAVE, not
# REPLICATION CLIENT; SHOW SLAVE HOSTS before 8.0.22 / on MariaDB)
mysql --batch --raw -e "SHOW REPLICAS;"

# Replication is down? Diagnose before proposing CHANGE REPLICATION SOURCE -
# read Last_IO_Error/Last_SQL_Error, compare server UUIDs and binlog positions.
# CHANGE REPLICATION SOURCE TO / RESET REPLICA are topology writes: user
# confirmation required, and never RESET REPLICA ALL on a healthy setup.
```

**Tablespace and file growth**:

```sql
-- Caveat: FILES reports InnoDB's cache of OPEN files and total_extents
-- ignores a trailing partial extent, so numbers skew low. For real disk
-- usage prefer innodb_tablespaces (or du on @@datadir)
SELECT file_name, ROUND(total_extents*extent_size/1024/1024) AS mb, status
FROM information_schema.files
WHERE engine = 'InnoDB' AND (file_name LIKE '%ibdata%' OR file_name LIKE '%ibtmp%'
   OR file_name LIKE '%undo%');
SELECT name, ROUND(file_size/1024/1024) AS file_mb,
       ROUND(allocated_size/1024/1024) AS alloc_mb
FROM information_schema.innodb_tablespaces
ORDER BY allocated_size DESC LIMIT 15;
```

Runaway `ibtmp1` or undo growth is a symptom (long transactions, missing
temp-table indexes) - report the finding and the driving queries, not just
the file size. `ibdata` cannot shrink online; any shrink plan is a
rebuild-class operation requiring its own change process.

**Key variables** (report, do not tune - tuning belongs to the user or
`ktuner` for host-level memory/kernel interplay):

```sql
SELECT @@innodb_buffer_pool_size AS buffer_pool,
       @@max_connections, @@innodb_flush_log_at_trx_commit, @@sync_binlog,
       @@table_open_cache, @@thread_cache_size;
```

Buffer pool vs host memory: compare with the OS view (`free -h`), not a
guess.

---

## Quick troubleshooting table

| Symptom | Locate with | Action |
|---------|-------------|--------|
| Lock wait timeout (1205) | `data_lock_waits` + `innodb_trx` join above | Show blocker and victim; propose `KILL` on confirmation |
| Deadlock (1213) | `SHOW ENGINE INNODB STATUS` LATEST DETECTED DEADLOCK section | Report the lock order; application-side fix |
| Too many connections (1040) | `mysqladmin --no-beep ping`; `Max_used_connections%` status | Follow the 1040 playbook below the table |
| Slow page / query | Digest summary, then `EXPLAIN` | Propose index/query changes; DDL on confirmation |
| Replica lag (`Seconds_Behind_Source` growing) | `SHOW REPLICA STATUS` + `performance_schema.replication_applier_status_by_worker` | Identify the bottleneck (IO vs SQL thread) before touching anything |
| Disk filling under datadir | `@@datadir` + `df -h` + capacity tables (the two capacity tables need `PROCESS`; `@@datadir` and `df`/`du` need none - without `PROCESS`, `df` + `du` remain available) | Find the growing object; cleanup is a confirmed write |
| Dump fails or fills disk | Space pre-checks above | Re-scope dump (schema subset), never kill mid-dump silently |
| Auth failure in step zero | `mysql -e "SELECT 1"` error | Ask the user for their auth path; no guessing |
| Empty result sets / ERROR 1227 / ERROR 1146 | Privilege probe + processlist source gate (step zero) | Privilege gap, P_S off / table missing (pre-8.0.22, MariaDB), or field-name version difference (`Replica_*` vs `Slave_*`) - use the fallback source, report the gap, never conclude from partial data |

**1040 playbook**: `mysqladmin --no-beep ping` first - it needs no SQL
session. If locked out, only a `CONNECTION_ADMIN`-privileged account can
use the reserved connection slot. Once connected (or from a peer), read
`Max_used_connections` / `Max_used_connections_time` to confirm the cap
was truly hit and when, then the processlist and per-account queries to
find who holds the connections. Raising `max_connections` is a confirmed
`SET GLOBAL` write - and the wrong fix if the leak is application-side.

More read-only commands: `references/diagnostics-quickref.md`. Before any
backup, restore, or kill: `references/backup-safety-checklist.md`.

---

## High-risk confirmation list

Show the user the impact and obtain explicit confirmation before any of:

1. `KILL <connection>` (rolls back the transaction; use `KILL QUERY` to
   cancel only the statement when the session must survive)
2. `DROP TABLE` / `DROP DATABASE` / `TRUNCATE` (data deletion; `TRUNCATE`
   cannot be rolled back and resets auto-increment)
3. `ALTER TABLE` on large tables (online-DDL still holds metadata locks;
   state ALGORITHM/LOCK expectations)
4. `RESET MASTER` / `RESET BINARY LOGS AND GTIDS` (destroys the binlog
   recovery chain; irreversible. `RESET MASTER` is the only spelling on
   MySQL 8.0, deprecated since 8.0.35; 8.2/8.4 rename it to
   `RESET BINARY LOGS AND GTIDS` and 8.4 removes the old spelling;
   MariaDB also uses `RESET MASTER`)
5. `CHANGE REPLICATION SOURCE TO` / `RESET REPLICA [ALL]` (topology change)
6. `SET GLOBAL` of any variable (affects all sessions immediately, lost on
   restart unless the config file is updated too)
7. Restoring a dump into an existing schema (overwrites objects of the same
   name)
8. Enabling `slow_query_log` or `general_log` on a busy production server
   (general_log especially can degrade performance and grows unbounded)
9. `EXPLAIN ANALYZE` on a plain `SELECT` over a large table - the full
   statement executes with no implicit LIMIT. (On write statements and
   `FOR UPDATE`/`FOR SHARE` it is never run at all - that is a refusal,
   not a confirmation; see the EXPLAIN rules in the slow query section
   and the Never list in the checklist)

For each confirmation provide: the exact affected objects, how to roll back
(or that rollback is impossible), and a less destructive alternative. Until
the user agrees, run read-only commands only.

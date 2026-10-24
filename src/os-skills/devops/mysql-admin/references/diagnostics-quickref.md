# MySQL diagnostics quick reference

Read-only commands, ordered global → object → detail. Every command in
the tables below is read-only and safe for production data; the gated
exceptions (statements that really execute) live in the final section,
never in the tables. One caveat: the processlist/I_S rows take a global
mutex on busy servers (see the source gate in SKILL.md step zero) -
prefer the P_S forms there. All invocations use
`mysql --batch --raw -e "..."` (or `--defaults-extra-file=<path>` when the
user supplies one); never open an interactive shell.

## Server identity and state

| Command | Purpose |
|---------|---------|
| `mysql --batch --raw -e "SELECT @@version, @@version_comment, @@hostname, @@port, @@socket;"` | Version (8.0 vs MariaDB shapes later command choice), host, port, socket |
| `mysql --batch --raw -e "SELECT @@read_only, @@super_read_only;"` | Whether writes are expected to fail here (replica / hardened primary) |
| `mysql --batch --raw -e "SELECT @@datadir, @@tmpdir, @@log_error;"` | Data, temp, and error-log locations from the server itself |
| `mysql --batch --raw -e "SHOW GLOBAL STATUS LIKE 'Uptime';"` | Stats observation window (index-usage stats reset on restart) |
| `mysqladmin ping` / `systemctl is-active mysqld mysql mariadb` | Liveness when SQL auth itself is the problem |
| `SHOW GRANTS;` + `SELECT COUNT(*) FROM information_schema.innodb_trx;` | Privilege probe: `PROCESS` gate for trx/processlist visibility (ERROR 1227 = report the gap, do not conclude from partial data) |

## Sessions and connections

| Command | Purpose |
|---------|---------|
| `mysql --batch --raw -e "SHOW PROCESSLIST;" \| head -40` | Current sessions; bound the output (global mutex on busy servers - I_S implementation) |
| `SELECT id,user,host,db,TIME AS seconds,state,LEFT(info,120) FROM performance_schema.processlist WHERE command<>'Sleep' AND TIME>60 ORDER BY TIME DESC LIMIT 20;` | Long runners only, oldest first (P_S form: 8.0.22+ with P_S on, probe in step zero; otherwise the I_S fallback - deprecated, global mutex) |
| `SELECT @@max_connections AS cap;` + `SHOW GLOBAL STATUS LIKE 'Threads_connected';` | Connection picture: cap vs currently connected - count via Threads_connected (no privilege, no P_S dependency, all versions; P_S COUNT(*) can silently undercount, see source gate) |
| `SHOW GLOBAL STATUS LIKE 'Max_used_connections%';` | High-water mark plus `Max_used_connections_time` - whether (and when) the cap was truly hit |
| `SHOW GLOBAL STATUS LIKE 'Aborted_connect%';` | Failing/aborted connection history |
| `SELECT user,host,COUNT(*) FROM performance_schema.processlist GROUP BY user,host ORDER BY 3 DESC LIMIT 10;` | Which accounts hold the connections (P_S detail query, not a count - subject to the source gate) |

## Locks and transactions

| Command | Purpose |
|---------|---------|
| `performance_schema.data_lock_waits` joined to `innodb_trx` (see SKILL.md) | Who blocks whom, with query text |
| `SELECT trx_id,trx_state,trx_started,trx_mysql_thread_id,LEFT(trx_query,120) FROM information_schema.innodb_trx ORDER BY trx_started LIMIT 10;` | Oldest open transactions |
| `SELECT @@innodb_lock_wait_timeout;` | How long victims wait before error 1205 |
| `mysql --batch --raw -e "SHOW ENGINE INNODB STATUS\G" \| sed -n '/^LATEST DETECTED DEADLOCK$/,/^TRANSACTIONS$/p' \| head -120` | Last deadlock only, ending at the TRANSACTIONS section (full output is tens of KB; `grep -E '^\*\*\* WE ROLL BACK'` when only the conclusion is needed) |
| `SELECT @@innodb_print_all_deadlocks;` | Whether deadlock history lands in the error log |
| `SELECT count(*) FROM performance_schema.data_locks;` | Overall lock pressure snapshot (MariaDB: `information_schema.innodb_locks`) |

## Slow queries and plans

| Command | Purpose |
|---------|---------|
| `SELECT @@slow_query_log, @@slow_query_log_file, @@long_query_time, @@log_output;` | Slow-log state and location (from the server, not a guessed path) |
| `performance_schema.events_statements_summary_by_digest` (see SKILL.md) | Top offenders by total time - works even when the slow log is off |
| `SHOW GLOBAL STATUS LIKE 'Slow_queries';` | Slow counter since restart |
| `EXPLAIN FORMAT=TREE <select>;` | Plan shape without executing |
| `command -v mysqldumpslow pt-query-digest` | Which offline analyzer exists for the log file |

## Index and table health

| Command | Purpose |
|---------|---------|
| `SELECT table_schema,table_name,redundant_index_name,dominant_index_name FROM sys.schema_redundant_indexes LIMIT 20;` | Redundant index pairs (bounded; MariaDB does not ship the sys schema - probe `SHOW TABLES FROM sys LIKE 'schema_%indexes';`) |
| `SELECT object_schema,object_name,index_name FROM sys.schema_unused_indexes WHERE object_schema NOT IN ('mysql','sys','performance_schema') LIMIT 20;` | Unused since restart (caveat: observation window; same MariaDB sys-schema gap) |
| `SELECT table_schema,table_name,ROUND((data_length+index_length)/1024/1024) AS mb,table_rows FROM information_schema.tables WHERE table_schema NOT IN ('mysql','sys','performance_schema','information_schema') ORDER BY (data_length+index_length) DESC LIMIT 15;` | Largest tables |

## Replication (MySQL 8.0.22+; MariaDB uses SHOW SLAVE STATUS)

| Command | Purpose |
|---------|---------|
| `SHOW REPLICA STATUS\G` \| `grep -Ei '(Replica\|Slave)_(IO\|SQL)_Running\|Seconds_Behind\|Last_.*Error'` | One-shot replica health - the grep matches both field-name generations (`Replica_*` 8.0.22+, `Slave_*` earlier/MariaDB) |
| `SELECT * FROM performance_schema.replication_connection_status;` | THIS instance as a replica: IO thread state and received position (empty on a pure source - not an error) |
| `SELECT * FROM performance_schema.replication_applier_status_by_worker;` | SQL thread state - the lag source when IO is healthy |
| `SHOW REPLICAS;` (`SHOW SLAVE HOSTS` before 8.0.22 / MariaDB; needs `REPLICATION SLAVE`) | Replicas attached to this source |
| `SHOW BINARY LOGS;` / `SHOW MASTER STATUS;` (source side; `SHOW BINARY LOG STATUS` from 8.2, old name removed in 8.4) | Binlog inventory and current write position |
| `SELECT @@log_bin, @@server_id;` + `SELECT @@gtid_mode;` (separate statements: `@@gtid_mode` errors on MariaDB and would take the whole compound statement down) + `SHOW VARIABLES LIKE 'binlog_format';` | Whether PITR and GTID tooling are available (8.0.34+ deprecates `binlog_format`, 8.4 removes it - the SHOW form returns empty instead of erroring there; MariaDB: no `gtid_mode`, see mapping table) |

## Capacity and files

| Command | Purpose |
|---------|---------|
| `SELECT name, ROUND(file_size/1024/1024) AS file_mb, ROUND(allocated_size/1024/1024) AS alloc_mb FROM information_schema.innodb_tablespaces ORDER BY allocated_size DESC LIMIT 15;` | Tablespace sizes (preferred; `information_schema.files` counts only open files and partial extents, so it skews low. MariaDB: `INNODB_SYS_TABLESPACES` lacks the size columns - use `df`/`du`) |
| `df -h "$(mysql --batch --raw -N -e 'SELECT @@datadir;')"` | Free space on the data filesystem |
| `SELECT @@innodb_buffer_pool_size;` + `free -h` | Buffer pool vs host memory (report only - sizing is the user's call) |
| `SHOW GLOBAL STATUS LIKE 'Innodb_buffer_pool_read%';` | Hit-rate ratio for the buffer pool |

## Not read-only (gated - never in the tables above)

| Command | Gate |
|---------|------|
| `EXPLAIN ANALYZE <select>;` | Really executes the statement: plain `SELECT`s only, user-confirmed on large/busy servers (no implicit LIMIT - the full statement runs). Multi-table `UPDATE`/`DELETE` really modify data and `FOR UPDATE`/`FOR SHARE` take row locks - refuse those outright, they are not confirmation-gated |

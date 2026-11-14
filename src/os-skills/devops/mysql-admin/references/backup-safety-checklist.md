# Backup and restore safety checklist

Walk through this checklist before any `mysqldump`, restore, `KILL`, or
binlog-resetting operation. If any item fails, stop and return to read-only
diagnostics.

## Before any dump (backup preflight)

- [ ] Privileges confirmed for the dump options in use (`SELECT`,
      `SHOW DATABASES`, `SHOW VIEW`, `TRIGGER`, `EVENT`, `RELOAD`,
      `REPLICATION CLIENT`; `RELOAD` or `FLUSH_TABLES` additionally
      required with `--single-transaction` when `gtid_mode=ON` and
      `--set-gtid-purged=ON|AUTO` on 8.0.32+; `PROCESS` on 8.0.21+
      unless `--no-tablespaces`) - a dump that dies mid-way on a missing
      grant is the failure mode this checklist exists to prevent
- [ ] No long write transactions in flight
      (`information_schema.innodb_trx`, needs `PROCESS`) - they extend
      the FTWRL window when `--source-data`/`--master-data` is used
- [ ] No DDL planned during the dump window (`ALTER/CREATE/DROP/RENAME/
      TRUNCATE TABLE` break the consistent read: contents can be wrong
      or the dump can fail) - online DDL scheduled? say so and reschedule
- [ ] Binlog state known: `@@log_bin` and restore-anchor capture decided
      (`--source-data=2` on MySQL 8.0.26+ clients, `--master-data=2` on
      MariaDB and MySQL < 8.0.26) - omit it only on a server with
      `@@log_bin = OFF`, or deliberately in the GTID seeding form
      (`--set-gtid-purged=ON`, where the anchor is the dump's GTID set -
      adding `--source-data` back would re-introduce the FTWRL window)
- [ ] `--single-transaction` used for InnoDB; if any MyISAM tables are in
      scope, their inconsistency window is stated to the user
- [ ] Storage engine mix checked (`information_schema.tables` GROUP BY
      engine) - surprise CSV/MyISAM tables change the consistency story
- [ ] Dump destination free space checked (`df -h <dest>`) against the
      data size estimate, with headroom for the SQL text expansion
- [ ] Scope decided explicitly: whole instance vs selected schemas vs
      selected tables; excluded objects (routines, events, triggers)
      stated if not dumped
- [ ] `--set-gtid-purged` value decided by the restore target (criterion
      in SKILL.md's backup section): `ON` only for a fresh or
      empty-`gtid_executed` target on MySQL 8.0.32+; otherwise `OFF`
      with `--source-data=2` - the user confirmed the choice
- [ ] Dump file name carries the date and scope; it lands on a filesystem
      with retention (not `/tmp`)

## Before any restore (highest-risk operation)

- [ ] Target database named explicitly - never restore blind on top of an
      existing production schema
- [ ] Dump completeness verified: mysqldump exit code 0 AND the trailing
      `-- Dump completed on` line present - a dump cut short by a full
      disk or a killed connection is never used for restore
- [ ] Dump header inspected (`grep -n -m5 -E '^(CREATE DATABASE|USE|DROP TABLE)'
      dump.sql`) for `CREATE DATABASE`, `USE`, and `DROP TABLE` statements -
      the user knows what the dump will overwrite (`head` is unreliable on
      dumps with routines/events)
- [ ] Restore anchor recorded for PITR: `--source-data` dumps carry the
      file/pos comment (grep the dump with
      `grep -E 'CHANGE (REPLICATION SOURCE TO|MASTER TO)|(SOURCE_LOG_FILE|MASTER_LOG_FILE)'` -
      the wording varies by client version); `--set-gtid-purged=ON` dumps
      carry `SET @@GLOBAL.gtid_purged` instead - record whichever applies
- [ ] Destination free space checked: dump size times a load factor for
      InnoDB growth and index rebuild
- [ ] Replica propagation consequence known: an `OFF` dump replays into
      the binlog and propagates to every replica; an `ON`/`AUTO`/
      `COMMENTED` dump (GTID-enabled source) contains
      `SET @@SESSION.sql_log_bin=0` - active even in `COMMENTED`, where
      only `gtid_purged` is commented out - so the restore does NOT
      propagate, and each replica then needs its own catch-up plan
- [ ] Rollback answer exists: the pre-restore state is itself backed up
      (schema export of the target at minimum) or the user accepted that
      restore is irreversible for the overwritten objects
- [ ] Restore tested as a count/row-level sanity check plan: row counts of
      key tables before declaring success

## Before KILL / destructive SQL

- [ ] Victim query shown with runtime and state (`processlist` entry)
- [ ] `KILL QUERY` considered first when the session should survive
- [ ] What the transaction rollback discards is stated
- [ ] For `DROP`/`TRUNCATE`: schema export of the object taken first
      (`mysqldump --no-data` at minimum, full dump preferred) and the user
      confirmed by exact object name

## During execution

- [ ] Dump/restore runs with output redirected to a log file, not streamed
      into the conversation
- [ ] One operation at a time; no parallel dumps competing for the same
      buffer pool on a small server
- [ ] On unexpected error (FK constraint, disk full mid-restore): stop and
      report; never retry blind

## After execution

- [ ] Row counts / `CHECKSUM TABLE` on key tables compared against
      expectations
- [ ] `SHOW REPLICA STATUS` re-checked if replication is in play
- [ ] Backup artifact location and binlog position reported to the user
      together - a dump without its position is only half a recovery
- [ ] Disk space re-checked after restore (InnoDB files do not shrink
      after rollback of a failed load)

## Never (refuse outright, no checklist)

- `RESET MASTER` (the only spelling on MySQL 8.0, deprecated 8.0.35;
  renamed `RESET BINARY LOGS AND GTIDS` in 8.2/8.4, old spelling removed
  in 8.4; MariaDB also `RESET MASTER`) - never as a side effect of
  another action, and never before showing the user exactly which binlog
  files and GTID ranges will be destroyed (it is confirmation-gated in
  SKILL.md's high-risk list)
- `EXPLAIN ANALYZE` on any statement that is not a plain `SELECT`:
  multi-table `UPDATE`/`DELETE` really execute and modify data, and
  `FOR UPDATE`/`FOR SHARE` take row locks - refuse outright, this is not
  a confirmation-gated action
- Putting a password on the mysql command line (`-p<password>` or
  `MYSQL_PWD=`) - visible in process lists and shell history
- Restoring a dump whose origin and target have not both been verified
- Deleting binlog files from the filesystem (`rm` on binlogs corrupts the
  index; use `PURGE BINARY LOGS` on explicit user instruction only)
- Any `--flush-logs` / `--delete-master-logs` dump option on a production
  primary without confirmation - it rotates and may purge binlogs replicas
  still need

//! Private, bounded projection of AW public objects and their durable chain checkpoints.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::path::Path;

use super::Storage;
use crate::database::{
    DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseRole, DatabaseSpec,
};

const FILE: &str = "audit.db";
const MAX_MIB: i64 = 64;

pub(crate) struct AwAuditStore(Connection);

pub(crate) struct Chain {
    pub(crate) id: String,
    pub(crate) cursor: Option<Value>,
    pub(crate) gap: Option<String>,
}

pub(crate) struct Page<'a> {
    pub(crate) cursor: &'a Value,
    pub(crate) done: bool,
    pub(crate) event: &'a Value,
    pub(crate) records: &'a [Value],
}

impl Storage {
    pub(crate) fn open_aw_audit(base: &Path) -> Result<AwAuditStore> {
        let path = base.join("aw-audit").join(FILE);
        let manager = DatabaseManager::new(
            DatabaseRole::Server,
            [DatabaseSpec::new(
                DatabaseId::AwAudit,
                path,
                DatabaseAccess::ReadWrite,
                DatabaseCoverage::Partial,
            )],
        )?;
        Ok(manager.open_read_write(DatabaseId::AwAudit, AwAuditStore::open)?)
    }

    pub(crate) fn query_aw_audit(base: &Path) -> Result<AwAuditStore> {
        let path = base.join("aw-audit").join(FILE);
        Ok(DatabaseManager::open_query(
            DatabaseId::AwAudit,
            path,
            DatabaseCoverage::Partial,
            |path| crate::private_sqlite::read_private_connection(path).map(AwAuditStore),
        )?)
    }
}

impl AwAuditStore {
    fn open(path: &Path) -> Result<Self> {
        let directory = path
            .parent()
            .context("AW audit database has no parent directory")?;
        let mut connection = crate::private_sqlite::open_private_connection(directory, FILE)?;
        let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "max_page_count", MAX_MIB * 1024 * 1024 / page_size)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS aw_chains (
                source TEXT NOT NULL, chain TEXT NOT NULL, cursor TEXT, done INTEGER NOT NULL DEFAULT 0,
                checked_at INTEGER NOT NULL DEFAULT 0, gap TEXT,
                PRIMARY KEY (source, chain)
             );
             CREATE TABLE IF NOT EXISTS aw_ingestion (
                source TEXT PRIMARY KEY, checked_at INTEGER NOT NULL, state TEXT NOT NULL, last_success_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS aw_ingestion_inputs (
                binary BLOB NOT NULL, state_dir BLOB NOT NULL, source TEXT NOT NULL,
                PRIMARY KEY (binary, state_dir)
             );
             CREATE INDEX IF NOT EXISTS aw_chains_pending
                ON aw_chains(source, checked_at, chain) WHERE done=0;
             CREATE TABLE IF NOT EXISTS aw_objects (
                source TEXT NOT NULL, uid TEXT NOT NULL, chain TEXT NOT NULL, sequence INTEGER NOT NULL,
                occurred_at INTEGER NOT NULL, instance TEXT NOT NULL, session TEXT, call TEXT, body TEXT NOT NULL,
                PRIMARY KEY (source, uid), UNIQUE (source, chain, sequence)
             );
             CREATE INDEX IF NOT EXISTS aw_objects_timeline ON aw_objects(source, occurred_at, chain, sequence);"
        )?;
        let has_last_success: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('aw_ingestion') WHERE name='last_success_at')",
            [],
            |row| row.get(0),
        )?;
        if !has_last_success {
            transaction.execute_batch(
                "ALTER TABLE aw_ingestion ADD COLUMN last_success_at INTEGER;
                 UPDATE aw_ingestion SET last_success_at=checked_at, state='unknown'
                    WHERE state='reachable';
                 DROP INDEX IF EXISTS aw_chains_schedule;",
            )?;
        }
        transaction.commit()?;
        Ok(Self(connection))
    }

    pub(crate) fn configured_source(
        &self,
        binary: &Path,
        state_dir: &Path,
    ) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row(
                "SELECT source FROM aw_ingestion_inputs WHERE binary=?1 AND state_dir=?2",
                params![
                    binary.as_os_str().as_encoded_bytes(),
                    state_dir.as_os_str().as_encoded_bytes()
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn bind_source(&self, binary: &Path, state_dir: &Path, source: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO aw_ingestion_inputs(binary,state_dir,source) VALUES (?1,?2,?3)
             ON CONFLICT(binary,state_dir) DO UPDATE SET source=excluded.source",
            params![
                binary.as_os_str().as_encoded_bytes(),
                state_dir.as_os_str().as_encoded_bytes(),
                source
            ],
        )?;
        Ok(())
    }

    pub(crate) fn status(&self, source: &str, state: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO aw_ingestion(source,checked_at,state,last_success_at)
             VALUES (?1, CAST(unixepoch('subsec')*1000 AS INTEGER), ?2,
                CASE WHEN ?2='reachable' THEN CAST(unixepoch('subsec')*1000 AS INTEGER) END)
             ON CONFLICT(source) DO UPDATE SET checked_at=excluded.checked_at,state=excluded.state,
                last_success_at=COALESCE(excluded.last_success_at,aw_ingestion.last_success_at)",
            params![source, state],
        )?;
        Ok(())
    }

    pub(crate) fn discover(&mut self, source: &str, keys: &[String]) -> Result<()> {
        let transaction = self.0.transaction()?;
        for key in keys {
            transaction.execute(
                "INSERT OR IGNORE INTO aw_chains(source, chain) VALUES (?1, ?2)",
                params![source, key],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn next_chains(&self, source: &str) -> Result<Vec<Chain>> {
        let mut statement = self.0.prepare_cached(
            "SELECT chain, cursor, gap FROM aw_chains WHERE source=?1 AND done=0 ORDER BY checked_at, chain LIMIT 4",
        )?;
        let mut chains = Vec::new();
        let mut rows = statement.query([source])?;
        while let Some(row) = rows.next()? {
            let cursor: Option<String> = row.get(1)?;
            chains.push(Chain {
                id: row.get(0)?,
                cursor: cursor.as_deref().map(serde_json::from_str).transpose()?,
                gap: row.get(2)?,
            });
        }
        Ok(chains)
    }

    pub(crate) fn gap(&self, source: &str, chain: &Chain, code: &str) -> Result<()> {
        self.0.execute(
            "UPDATE aw_chains SET gap=?1, checked_at=CAST(unixepoch('subsec')*1000 AS INTEGER) WHERE source=?2 AND chain=?3 AND done=0 AND cursor IS ?4",
            params![code, source, chain.id, chain.cursor.as_ref().map(Value::to_string)],
        )?;
        Ok(())
    }

    pub(crate) fn commit(&mut self, source: &str, chain: &Chain, page: Page<'_>) -> Result<()> {
        let transaction = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected = chain.cursor.as_ref().map(Value::to_string);
        let updated = transaction.execute(
            "UPDATE aw_chains SET cursor=?1, done=?2, gap=NULL, checked_at=CAST(unixepoch('subsec')*1000 AS INTEGER)
             WHERE source=?3 AND chain=?4 AND done=0 AND cursor IS ?5",
            params![page.cursor.to_string(), page.done, source, chain.id, expected],
        )?;
        if updated == 0 {
            return Ok(());
        }
        for (sequence, object) in
            std::iter::once((-1, page.event)).chain(page.records.iter().map(|record| {
                (
                    record["data"]["chain"]["sequence"].as_i64().unwrap_or(-1),
                    record,
                )
            }))
        {
            let meta = &object["metadata"];
            let subject = &object["data"]["subject"];
            let uid = meta["uid"].as_str().context("missing AW object identity")?;
            let created = meta["createdAt"]
                .as_str()
                .context("missing AW object timestamp")?;
            let occurred_at = chrono::DateTime::parse_from_rfc3339(created)?.timestamp_millis();
            let instance = subject["instanceId"]
                .as_str()
                .context("missing AW instance identity")?;
            let mut canonical = object.clone();
            canonical.sort_all_objects();
            let body = canonical.to_string();
            ensure!(
                body.len() <= 64 * 1024,
                "AW metadata object exceeds byte limit"
            );
            let previous: Option<String> = transaction
                .query_row(
                    "SELECT body FROM aw_objects WHERE source=?1 AND uid=?2",
                    params![source, uid],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(previous) = previous {
                ensure!(previous == body, "conflicting immutable AW audit object");
                continue;
            }
            transaction.execute(
                "INSERT INTO aw_objects(source,uid,chain,sequence,occurred_at,instance,session,call,body)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![source, uid, chain.id, sequence, occurred_at, instance,
                    subject["sessionId"].as_str(), subject["toolCallId"].as_str(), body],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn query(
        &self,
        source: &str,
        instance: Option<&str>,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Value> {
        let (total, pending, gaps, last_check_ms): (i64, i64, i64, i64) = self.0.query_row(
            "SELECT count(*), count(*) FILTER (WHERE done=0),
             count(*) FILTER (WHERE done=0 AND gap IS NOT NULL),
             COALESCE(max(checked_at),0) FROM aw_chains WHERE source=?1",
            [source],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let ingestion: Option<(i64, String, Option<i64>)> = self
            .0
            .query_row(
                "SELECT checked_at, state, last_success_at FROM aw_ingestion WHERE source=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let mut statement = self.0.prepare_cached(
            "SELECT body FROM aw_objects WHERE source=?1 AND (?2 IS NULL OR instance=?2)
             AND occurred_at>=?3 AND occurred_at<?4 ORDER BY occurred_at DESC, chain, sequence LIMIT 101",
        )?;
        let bodies = statement.query_map(params![source, instance, from_ms, to_ms], |row| {
            row.get::<_, String>(0)
        })?;
        let objects = bodies
            .map(|body| Ok(serde_json::from_str::<Value>(&body?)?))
            .collect::<Result<Vec<_>>>()?;
        let mut statement = self.0.prepare_cached(
            "SELECT chain, gap FROM aw_chains WHERE source=?1 AND done=0 AND gap IS NOT NULL
             ORDER BY checked_at DESC LIMIT 100",
        )?;
        let gaps_sample = statement
            .query_map([source], |row| {
                Ok(json!({
                    "chain_id": row.get::<_, String>(0)?, "code": row.get::<_, String>(1)?
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(
            json!({"source_id":source,"chains":total,"pending":pending,"gaps":gaps,
            "recent_gaps":gaps_sample, "last_check_ms":last_check_ms,
            "ingestion":ingestion.map(|(time,state,last_success)| json!({
                "checked_at_ms":time, "state":state, "last_success_at_ms":last_success
            })),
            "truncated":objects.len() > 100, "objects":&objects[..objects.len().min(100)]}),
        )
    }
}

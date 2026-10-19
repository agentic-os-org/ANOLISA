//! AgentSight-owned copies of queried Tokenless statistics in the GenAI database.

use std::collections::{BTreeSet, HashMap};

use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use super::GenAISqliteStore;
use crate::storage::sqlite::tokenless::TokenlessStatRow;

/// Persistence or validation failure for a queried statistics record.
#[derive(Debug, thiserror::Error)]
pub enum TokenlessRecordError {
    /// SQLite read or transaction failed.
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    /// A saved JSON record cannot be encoded or decoded.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Source data does not satisfy the statistics contract.
    #[error("invalid Tokenless record: {0}")]
    Invalid(&'static str),
    /// The store mutex was poisoned.
    #[error("Tokenless statistics store lock is poisoned")]
    Poisoned,
}

/// Saved record with an AgentSight-local identifier for detail queries.
#[derive(Debug, Serialize)]
pub struct SavedTokenlessRecord {
    /// Local identifier; distinct from the source record ID.
    pub cache_id: i64,
    /// Configured query endpoint that supplied the record.
    pub source: String,
    /// Original source fields, including content availability and missing values.
    pub record: Value,
    /// Local session matched by a unique captured tool-call ID; absent or ambiguous stays null.
    pub agentsight_session_id: Option<String>,
}

pub(super) fn init_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS tokenless_statistics (
        cache_id INTEGER PRIMARY KEY AUTOINCREMENT,
        source TEXT NOT NULL, record_id INTEGER NOT NULL, record_timestamp TEXT NOT NULL,
        timestamp_ns INTEGER NOT NULL, session_id TEXT, tool_use_id TEXT,
        content_included INTEGER NOT NULL, record_json TEXT NOT NULL,
        UNIQUE(source, record_id, record_timestamp));
        CREATE INDEX IF NOT EXISTS idx_tokenless_statistics_time ON tokenless_statistics(timestamp_ns);
        CREATE INDEX IF NOT EXISTS idx_tokenless_statistics_tool ON tokenless_statistics(tool_use_id);")
}

impl GenAISqliteStore {
    /// Save a query result atomically; metadata refreshes preserve already fetched content.
    ///
    /// # Errors
    /// Rejects invalid records and returns lock, serialization, or transaction errors.
    pub fn save_tokenless_records(
        &self,
        source: &str,
        records: &[Value],
    ) -> Result<(), TokenlessRecordError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| TokenlessRecordError::Poisoned)?;
        let tx = conn.transaction()?;
        for record in records {
            let id = record["id"]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or(TokenlessRecordError::Invalid("source ID"))?;
            let timestamp = record["timestamp"]
                .as_str()
                .ok_or(TokenlessRecordError::Invalid("timestamp"))?;
            let timestamp_ns = chrono::DateTime::parse_from_rfc3339(timestamp)
                .ok()
                .and_then(|time| time.timestamp_nanos_opt())
                .ok_or(TokenlessRecordError::Invalid("timestamp range"))?;
            for field in ["before_tokens", "after_tokens"] {
                if record[field].as_i64().is_none_or(|count| count < 0) {
                    return Err(TokenlessRecordError::Invalid("token counts"));
                }
            }
            for field in ["session_id", "tool_use_id", "mode"] {
                if !record[field].is_null() && !record[field].is_string() {
                    return Err(TokenlessRecordError::Invalid("attribution or mode"));
                }
            }
            let content = record["content_included"]
                .as_bool()
                .ok_or(TokenlessRecordError::Invalid("content availability"))?;
            tx.execute("INSERT INTO tokenless_statistics
                (source,record_id,record_timestamp,timestamp_ns,session_id,tool_use_id,content_included,record_json)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                ON CONFLICT(source,record_id,record_timestamp) DO UPDATE SET
                record_json=CASE WHEN excluded.content_included=1 OR tokenless_statistics.content_included=0
                    THEN excluded.record_json ELSE json_set(excluded.record_json,
                        '$.before_text',json_extract(tokenless_statistics.record_json,'$.before_text'),
                        '$.after_text',json_extract(tokenless_statistics.record_json,'$.after_text'),
                        '$.before_output',json_extract(tokenless_statistics.record_json,'$.before_output'),
                        '$.after_output',json_extract(tokenless_statistics.record_json,'$.after_output'),
                        '$.content_included',json('true')) END,
                session_id=excluded.session_id,tool_use_id=excluded.tool_use_id,
                content_included=MAX(tokenless_statistics.content_included,excluded.content_included)",
                params![source,id,timestamp,timestamp_ns,record["session_id"].as_str(),
                    record["tool_use_id"].as_str(),content,serde_json::to_string(record)?])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// List saved statistics in a time range, with optional exact attribution filters.
    ///
    /// # Errors
    /// Rejects invalid limits/ranges and returns lock, SQLite, or stored JSON errors.
    pub fn list_tokenless_records(
        &self,
        start_ns: i64,
        end_ns: i64,
        session: Option<&str>,
        tool: Option<&str>,
        limit: usize,
        before_cache_id: Option<i64>,
    ) -> Result<Vec<SavedTokenlessRecord>, TokenlessRecordError> {
        if start_ns > end_ns
            || limit == 0
            || limit > 100
            || before_cache_id.is_some_and(|id| id <= 0)
        {
            return Err(TokenlessRecordError::Invalid("query range or limit"));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| TokenlessRecordError::Poisoned)?;
        let mut stmt = conn.prepare(
            "SELECT cache_id,source,record_json FROM tokenless_statistics
            WHERE timestamp_ns >= ?1 AND timestamp_ns <= ?2
            AND (?3 IS NULL OR session_id=?3) AND (?4 IS NULL OR tool_use_id=?4)
            AND (?6 IS NULL OR cache_id < ?6) ORDER BY cache_id DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                start_ns,
                end_ns,
                session,
                tool,
                limit as i64,
                before_cache_id
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        let mut records = rows
            .map(|row| {
                let (cache_id, source, payload) = row?;
                let mut record: Value = serde_json::from_str(&payload)?;
                // A stored payload is external data; `Value::index_mut` panics
                // on a non-object, so reject the shape instead of aborting the
                // query thread.
                let Some(object) = record.as_object_mut() else {
                    return Err(TokenlessRecordError::Invalid("record is not a JSON object"));
                };
                // List responses must not disclose cached content implicitly.
                for field in ["before_text", "after_text", "before_output", "after_output"] {
                    object.insert(field.to_string(), Value::Null);
                }
                object.insert("content_included".to_string(), Value::Bool(false));
                Ok(SavedTokenlessRecord {
                    cache_id,
                    source,
                    agentsight_session_id: None,
                    record,
                })
            })
            .collect::<Result<Vec<_>, TokenlessRecordError>>()?;
        let ids: Vec<_> = records
            .iter()
            .filter_map(|row| row.record["tool_use_id"].as_str())
            .collect();
        let sessions = associated_sessions(&conn, &ids)?;
        for row in &mut records {
            row.agentsight_session_id = row.record["tool_use_id"]
                .as_str()
                .and_then(|id| sessions.get(id))
                .cloned();
        }
        Ok(records)
    }

    /// Read applied statistics, optionally restricted to the source records returned by this query.
    /// Previously saved records remain available separately when the source removes them.
    ///
    /// # Errors
    /// Returns serialization, database, or poisoned-lock failures.
    pub fn tokenless_applied_stats(
        &self,
        source: &str,
        ids: &[&str],
        start_ns: i64,
        end_ns: i64,
        current: Option<&[Value]>,
    ) -> Result<Vec<TokenlessStatRow>, TokenlessRecordError> {
        if start_ns > end_ns {
            return Err(TokenlessRecordError::Invalid("query range"));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| TokenlessRecordError::Poisoned)?;
        let current = current
            .map(|rows| {
                rows.iter()
                    .map(|row| (&row["id"], &row["timestamp"]))
                    .collect::<Vec<_>>()
            })
            .map(|keys| serde_json::to_string(&keys))
            .transpose()?;
        let mut stmt = conn.prepare(
            "SELECT record_json FROM tokenless_statistics
            WHERE source=?1 AND timestamp_ns >= ?2 AND timestamp_ns <= ?3
            AND json_extract(record_json,'$.mode')='active'
            AND tool_use_id IN (SELECT value FROM json_each(?4))
            AND (?5 IS NULL OR EXISTS (SELECT 1 FROM json_each(?5) fresh
                WHERE json_extract(fresh.value,'$[0]')=record_id
                AND json_extract(fresh.value,'$[1]')=record_timestamp))",
        )?;
        let mut rows = stmt
            .query_map(
                params![
                    source,
                    start_ns,
                    end_ns,
                    serde_json::to_string(ids)?,
                    current
                ],
                |row| row.get::<_, String>(0),
            )?
            .peekable();
        if rows.peek().is_none() {
            return Ok(Vec::new());
        }
        let sessions = associated_sessions(&conn, ids)?;
        let mut result = Vec::new();
        for row in rows {
            let record: Value = serde_json::from_str(&row?)?;
            let Some(session_id) = record["tool_use_id"]
                .as_str()
                .and_then(|id| sessions.get(id))
            else {
                continue;
            };
            result.push(TokenlessStatRow {
                session_id: session_id.clone(),
                tool_use_id: record["tool_use_id"].as_str().unwrap_or_default().into(),
                operation: record["operation"].as_str().unwrap_or("unknown").into(),
                before_tokens: record["before_tokens"]
                    .as_i64()
                    .ok_or(TokenlessRecordError::Invalid("before tokens"))?,
                after_tokens: record["after_tokens"]
                    .as_i64()
                    .ok_or(TokenlessRecordError::Invalid("after tokens"))?,
                before_text: record["before_text"].as_str().map(str::to_owned),
                after_text: record["after_text"].as_str().map(str::to_owned),
            });
        }
        Ok(result)
    }

    /// Read one saved record, including previously requested content.
    ///
    /// # Errors
    /// Returns lock, SQLite, or stored JSON failures; absent local IDs return `None`.
    pub fn tokenless_record(
        &self,
        cache_id: i64,
    ) -> Result<Option<SavedTokenlessRecord>, TokenlessRecordError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| TokenlessRecordError::Poisoned)?;
        let mut stmt =
            conn.prepare("SELECT source,record_json FROM tokenless_statistics WHERE cache_id=?1")?;
        let mut rows = stmt.query([cache_id])?;
        rows.next()?
            .map(|row| {
                let record: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
                Ok(SavedTokenlessRecord {
                    cache_id,
                    source: row.get(0)?,
                    agentsight_session_id: associated_session(&conn, &record)?,
                    record,
                })
            })
            .transpose()
    }
}

fn associated_session(
    conn: &Connection,
    record: &Value,
) -> Result<Option<String>, TokenlessRecordError> {
    let Some(id) = record["tool_use_id"].as_str() else {
        return Ok(None);
    };
    Ok(associated_sessions(conn, &[id])?.remove(id))
}

fn associated_sessions(
    conn: &Connection,
    ids: &[&str],
) -> Result<HashMap<String, String>, TokenlessRecordError> {
    let ids: BTreeSet<_> = ids.iter().copied().collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    // CROSS JOIN keeps JSON expansion outside the requested-ID loop, avoiding a scan per ID.
    let mut stmt = conn.prepare(
        "WITH requested AS MATERIALIZED (SELECT value AS tool_id FROM json_each(?1)),
        matches AS (
            SELECT requested.tool_id,event.session_id FROM requested
            JOIN genai_events event ON event.call_id=requested.tool_id
            WHERE event.event_type='llm_call' AND event.session_id IS NOT NULL
            UNION ALL
            SELECT requested.tool_id,event.session_id FROM genai_events event
            CROSS JOIN json_each(CASE WHEN json_valid(event.tool_call_ids)
                THEN event.tool_call_ids ELSE '[]' END) tool
            CROSS JOIN requested
            WHERE requested.tool_id=tool.value
            AND event.event_type='llm_call' AND event.session_id IS NOT NULL)
        SELECT tool_id,MIN(session_id) FROM matches GROUP BY tool_id
        HAVING COUNT(DISTINCT session_id)=1",
    )?;
    let rows = stmt.query_map([serde_json::to_string(&ids)?], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PeriodicStoragePolicy;
    use serde_json::json;

    #[test]
    fn batched_associations_preserve_unique_sessions_and_record_order() {
        let dir = std::env::temp_dir().join(format!("stats-batch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai.db"),
            PeriodicStoragePolicy::default(),
        )
        .unwrap();
        let ids: Vec<_> = (0..100).map(|id| format!("tool-{id}")).collect();
        {
            let conn = store.conn.lock().unwrap();
            for (index, id) in ids.iter().enumerate() {
                for _ in 0..2 {
                    conn.execute(
                        "INSERT INTO genai_events (event_type,call_id,session_id,start_timestamp_ns,tool_call_ids,event_json)
                        VALUES ('llm_call',?1,'local',1,?2,'{}')",
                        params![format!("call-{index}"), json!([id, id]).to_string()],
                    ).unwrap();
                }
            }
            for (kind, call, session, tools) in [
                ("llm_call", "direct", Some("direct-session"), "invalid JSON"),
                ("llm_call", "conflict", Some("first"), "[]"),
                ("llm_call", "other", Some("second"), "[\"conflict\"]"),
                ("llm_call", "null-session", None, "[\"tool-0\"]"),
                ("tool_call", "non-llm", Some("ignored"), "[\"tool-0\"]"),
                (
                    "llm_call",
                    "json-types",
                    Some("json-session"),
                    "[1,\"1\",null]",
                ),
                (
                    "llm_call",
                    "object",
                    Some("object-session"),
                    "{\"key\":\"object-tool\"}",
                ),
                (
                    "llm_call",
                    "scalar",
                    Some("scalar-session"),
                    "\"scalar-tool\"",
                ),
                (
                    "llm_call",
                    "quoted",
                    Some("quoted-session"),
                    "[\"tool' OR 1=1\"]",
                ),
                ("llm_call", "empty", Some("empty-session"), "[\"\"]"),
            ] {
                conn.execute(
                    "INSERT INTO genai_events (event_type,call_id,session_id,start_timestamp_ns,tool_call_ids,event_json)
                    VALUES (?1,?2,?3,1,?4,'{}')", params![kind, call, session, tools],
                ).unwrap();
            }
            let mut requested: Vec<_> = ids.iter().map(String::as_str).collect();
            requested.extend([
                "tool-0",
                "direct",
                "conflict",
                "missing",
                "1",
                "object-tool",
                "scalar-tool",
                "tool' OR 1=1",
                "",
            ]);
            let batch = associated_sessions(&conn, &requested).unwrap();
            assert_eq!(batch.len(), 106);
            for id in requested {
                let expected: Option<String> = conn.query_row(
                    "SELECT CASE WHEN COUNT(DISTINCT session_id)=1 THEN MIN(session_id) END
                    FROM genai_events WHERE event_type='llm_call' AND session_id IS NOT NULL
                    AND (?1=call_id OR EXISTS (SELECT 1 FROM json_each(
                        CASE WHEN json_valid(tool_call_ids) THEN tool_call_ids ELSE '[]' END) WHERE value=?1))",
                    [id], |row| row.get(0),
                ).unwrap();
                assert_eq!(
                    batch.get(id),
                    expected.as_ref(),
                    "association changed for {id:?}"
                );
            }
            assert!(associated_sessions(&conn, &[]).unwrap().is_empty());
            assert_eq!(associated_session(&conn, &json!({})).unwrap(), None);
        }
        let mut records: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(id, tool)| {
                json!({
                    "id":id + 1, "timestamp":"2026-01-01T00:00:00Z", "tool_use_id":tool,
                    "mode":"active", "before_tokens":10, "after_tokens":2, "content_included":false,
                })
            })
            .collect();
        for (id, tool) in [
            (101, Value::Null),
            (102, json!("missing")),
            (103, json!("conflict")),
        ] {
            records.push(
                json!({"id":id,"timestamp":"2026-01-01T00:00:00Z","tool_use_id":tool,
                "mode":"active","before_tokens":10,"after_tokens":2,"content_included":false}),
            );
        }
        store.save_tokenless_records("fixture", &records).unwrap();
        let first = store
            .list_tokenless_records(0, i64::MAX, None, None, 100, None)
            .unwrap();
        assert_eq!(first.len(), 100);
        assert!(
            first
                .windows(2)
                .all(|rows| rows[0].cache_id > rows[1].cache_id)
        );
        assert!(
            first[..3]
                .iter()
                .all(|row| row.agentsight_session_id.is_none())
        );
        assert!(
            first[3..]
                .iter()
                .all(|row| row.agentsight_session_id.as_deref() == Some("local"))
        );
        let second = store
            .list_tokenless_records(0, i64::MAX, None, None, 100, Some(first[99].cache_id))
            .unwrap();
        assert_eq!(second.len(), 3);
        let mut requested: Vec<_> = ids.iter().map(String::as_str).collect();
        requested.extend(["missing", "conflict"]);
        let applied = store
            .tokenless_applied_stats("fixture", &requested, 0, i64::MAX, None)
            .unwrap();
        assert_eq!(applied.len(), 100);
        assert!(applied.iter().all(|row| row.session_id == "local"));
        assert_eq!(
            applied
                .iter()
                .map(|row| row.before_tokens - row.after_tokens)
                .sum::<i64>(),
            800
        );
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deduplication_atomicity_content_and_restart() {
        let dir = std::env::temp_dir().join(format!("agentsight-stats-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("genai.db");
        let store =
            GenAISqliteStore::new_with_path(&path, PeriodicStoragePolicy::default()).unwrap();
        let metadata = json!({"id":1,"timestamp":"2026-01-01T00:00:00Z","session_id":"s","tool_use_id":"t",
            "before_tokens":6,"after_tokens":2,"mode":null,"content_included":false});
        store
            .save_tokenless_records("fixture", &[metadata.clone(), metadata.clone()])
            .unwrap();
        let list = store
            .list_tokenless_records(0, i64::MAX, Some("s"), Some("t"), 100, None)
            .unwrap();
        assert_eq!(list.len(), 1);
        let cache_id = list[0].cache_id;
        let mut detail = metadata.clone();
        detail["content_included"] = json!(true);
        detail["before_text"] = json!("sensitive fixture");
        store.save_tokenless_records("fixture", &[detail]).unwrap();
        store
            .save_tokenless_records("fixture", std::slice::from_ref(&metadata))
            .unwrap();
        let mut refreshed = metadata.clone();
        refreshed["after_tokens"] = json!(1);
        store
            .save_tokenless_records("fixture", &[refreshed])
            .unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO genai_events
            (event_type,call_id,session_id,start_timestamp_ns,tool_call_ids,event_json)
            VALUES ('llm_call','call','local-session',1,'[\"t\"]','{}')",
                [],
            )
            .unwrap();
        let saved = store.tokenless_record(cache_id).unwrap().unwrap();
        assert_eq!(saved.record["before_text"], "sensitive fixture");
        assert_eq!(saved.record["after_tokens"], 1);
        assert_eq!(
            saved.agentsight_session_id.as_deref(),
            Some("local-session")
        );
        let mut invalid = metadata.clone();
        invalid["id"] = json!(2);
        invalid["before_tokens"] = json!(-1);
        assert!(
            store
                .save_tokenless_records("other", &[metadata.clone(), invalid])
                .is_err()
        );
        let rows = store
            .list_tokenless_records(0, i64::MAX, None, None, 100, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        let mut reused = metadata;
        reused["timestamp"] = json!("2026-01-02T00:00:00Z");
        store.save_tokenless_records("fixture", &[reused]).unwrap();
        drop(store);
        let reopened =
            GenAISqliteStore::new_with_path(&path, PeriodicStoragePolicy::default()).unwrap();
        let rows = reopened
            .list_tokenless_records(0, i64::MAX, None, None, 100, None)
            .unwrap();
        assert_eq!(rows.len(), 2);
        let excluded = reopened
            .list_tokenless_records(0, 1, None, None, 100, None)
            .unwrap();
        assert!(excluded.is_empty());
        assert_eq!(
            reopened.tokenless_record(cache_id).unwrap().unwrap().record["before_text"],
            "sensitive fixture"
        );
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn saved_statistics_follow_retention_and_capacity_cleanup() {
        let directory =
            std::env::temp_dir().join(format!("stats-retention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let store = GenAISqliteStore::new_with_path(
            &directory.join("genai.db"),
            PeriodicStoragePolicy::new(1, 0, 60),
        )
        .unwrap();
        let mut record = json!({"id":1,"timestamp":"2020-01-01T00:00:00Z",
            "before_tokens":6,"after_tokens":2,"content_included":false});
        store
            .save_tokenless_records("fixture", std::slice::from_ref(&record))
            .unwrap();
        store.maintain().unwrap();
        assert!(
            store
                .list_tokenless_records(0, i64::MAX, None, None, 100, None)
                .unwrap()
                .is_empty()
        );
        record["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
        store.save_tokenless_records("fixture", &[record]).unwrap();
        assert_eq!(store.prune_old_records().unwrap(), 1);
        assert!(
            store
                .list_tokenless_records(0, i64::MAX, None, None, 100, None)
                .unwrap()
                .is_empty()
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn tokenless_maintenance_obeys_row_byte_and_expiry_bounds() {
        use super::super::schema::PruneTable;
        let dir = std::env::temp_dir().join(format!("stats-batches-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let store = GenAISqliteStore::new_with_path(
            &dir.join("stats.db"),
            PeriodicStoragePolicy::default(),
        )
        .unwrap();
        let records: Vec<_> = (1..=260)
            .map(|id| {
                json!({"id":id,
            "timestamp":"2020-01-01T00:00:00Z", "before_tokens":6,"after_tokens":2,"content_included":false})
            })
            .collect();
        store.save_tokenless_records("fixture", &records).unwrap();
        assert_eq!(
            store
                .delete_maintenance_batch(PruneTable::Tokenless, Some(1), usize::MAX)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .delete_maintenance_batch(PruneTable::Tokenless, None, usize::MAX)
                .unwrap(),
            128
        );
        assert_eq!(
            store
                .delete_maintenance_batch(PruneTable::Tokenless, None, 2)
                .unwrap(),
            2
        );
        store
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM tokenless_statistics", [])
            .unwrap();
        let oversized = json!({"id":1,"timestamp":"2020-01-01T00:00:00Z",
            "before_tokens":6,"after_tokens":2,"content_included":true,"before_text":"x".repeat(5*1024*1024)});
        store
            .save_tokenless_records("fixture", &[oversized])
            .unwrap();
        let small = json!({"id":2,"timestamp":"2020-01-01T00:00:00Z","before_tokens":6,"after_tokens":2,"content_included":false});
        store.save_tokenless_records("fixture", &[small]).unwrap();
        assert_eq!(
            store
                .delete_maintenance_batch(PruneTable::Tokenless, None, usize::MAX)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .delete_maintenance_batch(PruneTable::Tokenless, Some(i64::MAX), usize::MAX)
                .unwrap(),
            1
        );
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn applied_modes_sources_filters_and_pagination() {
        let dir = std::env::temp_dir().join(format!("stats-modes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai.db"),
            PeriodicStoragePolicy::default(),
        )
        .unwrap();
        for (id, mode) in [
            (1, json!("active")),
            (2, json!("dry-run")),
            (3, Value::Null),
            (4, json!("future")),
        ] {
            let record = json!({"id":id,"timestamp":"2026-01-01T00:00:00Z","mode":mode,
                "session_id":"source-session","tool_use_id":"tool","before_tokens":10,"after_tokens":2,"content_included":false});
            store
                .save_tokenless_records("source", std::slice::from_ref(&record))
                .unwrap();
            store
                .save_tokenless_records("other-source", &[record])
                .unwrap();
        }
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO genai_events
            (event_type,call_id,session_id,start_timestamp_ns,tool_call_ids,event_json)
            VALUES ('llm_call','call','local',1,'[\"tool\"]','{}')",
                [],
            )
            .unwrap();
        let rows = store
            .tokenless_applied_stats("source", &["tool"], 0, i64::MAX, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].before_tokens - rows[0].after_tokens, 8);
        assert!(
            store
                .tokenless_applied_stats("source", &["tool"], 0, i64::MAX, Some(&[]))
                .unwrap()
                .is_empty()
        );
        let current = json!({"id":1,"timestamp":"2026-01-01T00:00:00Z"});
        assert_eq!(
            store
                .tokenless_applied_stats("source", &["tool"], 0, i64::MAX, Some(&[current]))
                .unwrap()
                .len(),
            1
        );
        let replaced = json!({"id":1,"timestamp":"2026-01-02T00:00:00Z"});
        assert!(
            store
                .tokenless_applied_stats("source", &["tool"], 0, i64::MAX, Some(&[replaced]))
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .tokenless_applied_stats("source", &["tool' OR 1=1"], 0, i64::MAX, None)
                .unwrap()
                .is_empty()
        );
        let first = store
            .list_tokenless_records(0, i64::MAX, None, None, 1, None)
            .unwrap();
        let second = store
            .list_tokenless_records(0, i64::MAX, None, None, 1, Some(first[0].cache_id))
            .unwrap();
        assert!(second[0].cache_id < first[0].cache_id);
        assert!(
            store
                .list_tokenless_records(0, i64::MAX, None, None, 1, Some(0))
                .is_err()
        );
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO genai_events
            (event_type,call_id,session_id,start_timestamp_ns,tool_call_ids,event_json)
            VALUES ('llm_call','other-call','ambiguous',2,'[\"tool\"]','{}')",
                [],
            )
            .unwrap();
        assert!(
            store
                .tokenless_applied_stats("source", &["tool"], 0, i64::MAX, None)
                .unwrap()
                .is_empty()
        );
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn list_rejects_a_non_object_record_without_panicking() {
        // record_json is external data. Valid JSON that is not an object (a
        // top-level array or scalar) used to hit `Value::index_mut`, which
        // panics instead of returning the error this query already models.
        let dir = std::env::temp_dir().join(format!("stats-shape-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let store = GenAISqliteStore::new_with_path(
            &dir.join("genai.db"),
            PeriodicStoragePolicy::default(),
        )
        .unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO tokenless_statistics
                 (source,record_id,record_timestamp,timestamp_ns,content_included,record_json)
                 VALUES ('fixture',1,'2026-01-01T00:00:00Z',1,0,'[]')",
                [],
            )
            .unwrap();

        let result = store.list_tokenless_records(0, i64::MAX, None, None, 10, None);

        assert!(matches!(result, Err(TokenlessRecordError::Invalid(_))));
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

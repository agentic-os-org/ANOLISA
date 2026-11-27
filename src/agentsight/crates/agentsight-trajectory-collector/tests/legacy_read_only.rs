//! Serving a database must leave schema upgrades to the collector.

use std::path::Path;

use agentsight_trajectory_collector::TrajectoryStore;
use anyhow::Result;
use rusqlite::{types::Value, Connection, OpenFlags};

const LEGACY_SCHEMA: &str = "
    CREATE TABLE collected_trajectories (
        session_id TEXT PRIMARY KEY,
        schema_version TEXT NOT NULL,
        agent_name TEXT NOT NULL,
        model_name TEXT,
        num_steps INTEGER NOT NULL,
        total_prompt_tokens INTEGER,
        total_completion_tokens INTEGER,
        start_time TEXT,
        end_time TEXT,
        atif_json TEXT NOT NULL,
        project TEXT NOT NULL,
        source TEXT NOT NULL,
        is_subagent INTEGER NOT NULL,
        file_path TEXT NOT NULL,
        file_size INTEGER NOT NULL,
        file_mtime_ns INTEGER NOT NULL,
        collected_at_ns INTEGER NOT NULL
    );
    CREATE TABLE skipped_files (
        file_path TEXT PRIMARY KEY,
        file_size INTEGER NOT NULL,
        file_mtime_ns INTEGER NOT NULL
    );";

const ATIF: &str = r#"{"schema_version":"ATIF-v1.7","session_id":"s1","agent":{"name":"qoder","version":"1"},"steps":[{"step_id":1,"source":"user","message":"first"},{"step_id":2,"source":"user","message":"last"}]}"#;

fn schema(path: &Path) -> Result<(i32, Vec<Vec<Value>>)> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let mut stmt = conn.prepare("PRAGMA table_info(collected_trajectories)")?;
    let rows = stmt.query_map([], |row| {
        (0..row.as_ref().column_count())
            .map(|i| row.get(i))
            .collect()
    })?;
    Ok((version, rows.collect::<rusqlite::Result<_>>()?))
}

fn assert_read_only(path: &Path, first: Option<&str>, last: Option<&str>) -> Result<()> {
    let before = schema(path)?;
    let store = TrajectoryStore::open_read_only_existing(path)?;
    let summaries = store.list_summaries(Some("project"), Some("qoder"), None, 10)?;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].session_id, "s1");
    assert_eq!(summaries[0].first_user_message.as_deref(), first);
    assert_eq!(summaries[0].last_user_message.as_deref(), last);
    let record = store.get("s1")?.unwrap();
    assert_eq!(record.atif_json, ATIF);
    assert_eq!(record.first_user_message.as_deref(), first);
    assert_eq!(record.last_user_message.as_deref(), last);
    assert_eq!(store.count()?, 1);
    assert_eq!(
        store.list_recent_atif_jsons(0, 1)?,
        [("s1".into(), ATIF.into())]
    );
    assert!(store.set_file_state("s1.jsonl", 2, 3).is_err());
    drop(store);
    assert_eq!(schema(path)?, before);
    Ok(())
}

#[test]
fn read_only_previews_before_and_after_migration() -> Result<()> {
    let dir = std::env::temp_dir().join(format!(
        "agentsight-legacy-read-only-{}",
        std::process::id()
    ));
    std::fs::create_dir(&dir)?;
    let path = dir.join("trajectories.db");
    let conn = Connection::open(&path)?;
    conn.execute_batch(LEGACY_SCHEMA)?;
    conn.execute(
        "INSERT INTO collected_trajectories
         (session_id, schema_version, agent_name, num_steps, atif_json, project, source,
          is_subagent, file_path, file_size, file_mtime_ns, collected_at_ns)
         VALUES ('s1', 'ATIF-v1.7', 'qoder', 2, ?1, 'project', 'qoder', 0, 's1.jsonl', 1, 1, 1)",
        [ATIF],
    )?;
    drop(conn);
    let bytes = std::fs::read(&path)?;
    assert_read_only(&path, None, None)?;
    assert_eq!(std::fs::read(&path)?, bytes);

    // A migration interrupted between the two ALTERs still exposes the stored preview.
    Connection::open(&path)?.execute_batch(
        "ALTER TABLE collected_trajectories ADD COLUMN first_user_message TEXT;
         UPDATE collected_trajectories SET first_user_message = 'stored';",
    )?;
    assert_read_only(&path, Some("stored"), None)?;

    drop(TrajectoryStore::new_with_path(&path)?);
    assert_read_only(&path, Some("first"), Some("last"))?;
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

use super::*;
use crate::{CompressionMode, OperationType, StatsRecord, StatsRecorder};

fn legacy(conn: &Connection) {
    // The bundled RTK writer can be the first producer to create the database.
    conn.execute_batch("CREATE TABLE stats (
        id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp TEXT NOT NULL,
        operation TEXT NOT NULL, agent_id TEXT NOT NULL, source_pid INTEGER,
        session_id TEXT, tool_use_id TEXT, before_chars INTEGER NOT NULL,
        before_tokens INTEGER NOT NULL, after_chars INTEGER NOT NULL,
        after_tokens INTEGER NOT NULL, before_text TEXT, after_text TEXT);
        INSERT INTO stats (timestamp,operation,agent_id,session_id,tool_use_id,
        before_chars,before_tokens,after_chars,after_tokens,before_text,after_text)
        VALUES ('2026-01-01T00:00:00Z','rewrite-command','fixture','s','t',12,6,4,2,'original','short');").unwrap();
}

#[test]
fn legacy_fields_content_and_no_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let conn = Connection::open(&path).unwrap();
    legacy(&conn);
    let bytes = std::fs::read(&path).unwrap();
    let reader = StatsReader::open(&path).unwrap();
    let page = reader.list(&RecordQuery::default()).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0]["before_text"], Value::Null);
    assert_eq!(page.records[0]["mode"], Value::Null);
    assert_eq!(page.records[0]["content_included"], false);
    let record = reader.get(1, true).unwrap().unwrap();
    assert_eq!(record["before_text"], "original");
    assert_eq!(record["after_text"], "short");
    assert_eq!(record["before_output"], Value::Null);
    assert_eq!(record["timestamp"], "2026-01-01T00:00:00Z");
    assert_eq!(record["content_included"], true);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(reader.conn.execute("DELETE FROM stats", []).is_err());
}

#[test]
fn missing_and_unsupported_sources_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.db");
    assert!(StatsReader::open(&path).is_err());
    assert!(!path.exists());
    let conn = Connection::open(&path).unwrap();
    conn.execute("CREATE TABLE stats (id INTEGER)", []).unwrap();
    assert!(matches!(
        StatsReader::open(&path),
        Err(ReadError::Unsupported(_))
    ));
}

#[test]
fn live_wal_pagination_filters_and_producer_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let writer = StatsRecorder::new(&path).unwrap();
    let base = StatsRecord::new(OperationType::RewriteCommand, "fixture".into(), 12, 6, 4, 2)
        .with_session_id("session' OR 1=1 --")
        .with_tool_use_id("tool")
        .with_output("before\n世界".into(), "after".into());
    writer.record(&base).unwrap();
    writer
        .record(&base.clone().with_mode(CompressionMode::DryRun))
        .unwrap();
    let reader = StatsReader::open(&path).unwrap();
    let query = RecordQuery {
        limit: 1,
        ..Default::default()
    };
    let first = reader.list(&query).unwrap();
    assert_eq!(first.records[0]["id"], 2);
    assert_eq!(first.records[0]["mode"], "dry-run");
    assert_eq!(first.next_before_id, Some(2));
    writer.record(&base).unwrap();
    let next = reader
        .list(&RecordQuery {
            before_id: first.next_before_id,
            ..query.clone()
        })
        .unwrap();
    assert_eq!(next.records[0]["id"], 1);
    assert_eq!(next.next_before_id, None);
    assert_eq!(
        reader.list(&RecordQuery::default()).unwrap().records.len(),
        3
    );
    let filtered = RecordQuery {
        session_id: Some("session' OR 1=1 --".into()),
        tool_use_id: Some("tool".into()),
        ..Default::default()
    };
    assert_eq!(reader.list(&filtered).unwrap().records.len(), 3);
    assert!(
        reader
            .list(&RecordQuery {
                tool_use_id: Some("wrong".into()),
                ..filtered
            })
            .unwrap()
            .records
            .is_empty()
    );
    assert!(reader.get(99, true).unwrap().is_none());
    drop(writer);
    drop(reader);
    let reader = StatsReader::open(&path).unwrap();
    assert_eq!(
        reader.get(1, true).unwrap().unwrap()["before_output"],
        "before\n世界"
    );
    assert_eq!(
        reader.get(1, false).unwrap().unwrap()["before_output"],
        Value::Null
    );
}

#[test]
fn corrupt_records_and_invalid_queries_do_not_become_empty_successes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let writer = Connection::open(&path).unwrap();
    legacy(&writer);
    let reader = StatsReader::open(&path).unwrap();
    for limit in [0, 101, usize::MAX] {
        assert!(matches!(
            reader.list(&RecordQuery {
                limit,
                ..Default::default()
            }),
            Err(ReadError::InvalidQuery(_))
        ));
    }
    assert!(reader.get(0, false).is_err());
    writer
        .execute("UPDATE stats SET timestamp = 'corrupt'", [])
        .unwrap();
    assert!(matches!(
        reader.list(&RecordQuery::default()),
        Err(ReadError::InvalidRecord { .. })
    ));
    assert!(matches!(
        reader.get(1, false),
        Err(ReadError::InvalidRecord { .. })
    ));
    writer
        .execute(
            "UPDATE stats SET timestamp='2026-01-01T00:00:00Z', before_tokens=-1",
            [],
        )
        .unwrap();
    assert!(reader.list(&RecordQuery::default()).is_err());
}

#[test]
fn unknown_modes_and_operations_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let writer = Connection::open(&path).unwrap();
    legacy(&writer);
    writer.execute_batch("ALTER TABLE stats ADD COLUMN mode TEXT;
        UPDATE stats SET mode='future-mode', operation='future-operation', session_id=NULL, tool_use_id=NULL;").unwrap();
    let reader = StatsReader::open(&path).unwrap();
    let record = reader.get(1, false).unwrap().unwrap();
    assert_eq!(record["mode"], "future-mode");
    assert_eq!(record["operation"], "future-operation");
    assert_eq!(record["session_id"], Value::Null);
    assert_eq!(record["tool_use_id"], Value::Null);
}

#[test]
fn absent_optional_columns_and_database_locks_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE stats (id INTEGER PRIMARY KEY, timestamp TEXT,
        operation TEXT, agent_id TEXT, before_chars INTEGER, before_tokens INTEGER,
        after_chars INTEGER, after_tokens INTEGER);
        INSERT INTO stats VALUES (1,'2026-01-01T00:00:00Z','rewrite-command','fixture',12,6,4,2);",
        )
        .unwrap();
    let reader = StatsReader::open(&path).unwrap();
    assert!(matches!(
        reader.list_for_tools(&RecordQuery::default(), &["tool".into()]),
        Err(ReadError::Unsupported(_))
    ));
    assert_eq!(
        reader.list(&RecordQuery::default()).unwrap().records.len(),
        1
    );
    assert!(matches!(
        reader.list(&RecordQuery {
            session_id: Some("s".into()),
            ..Default::default()
        }),
        Err(ReadError::Unsupported(_))
    ));
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(matches!(
        reader.list(&RecordQuery::default()),
        Err(ReadError::Database(_))
    ));
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        reader.list(&RecordQuery::default()).unwrap().records.len(),
        1
    );
}

#[test]
fn batch_filters_paginate_without_duplicates_or_source_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let writer = Connection::open(&path).unwrap();
    legacy(&writer);
    for i in 0..120 {
        writer.execute("INSERT INTO stats(timestamp,operation,agent_id,tool_use_id,before_chars,before_tokens,after_chars,after_tokens)
            VALUES ('2026-01-01T00:00:00Z','rewrite-command','fixture',?1,12,6,4,2)", [format!("tool-{}",i%3)]).unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let reader = StatsReader::open(&path).unwrap();
    let ids = [
        "tool-1".into(),
        "tool-2".into(),
        "tool-1".into(),
        "tool' OR 1=1 --".into(),
    ];
    let first = reader
        .list_for_tools(&RecordQuery::default(), &ids)
        .unwrap();
    assert_eq!(first.records.len(), 50);
    let second = reader
        .list_for_tools(
            &RecordQuery {
                before_id: first.next_before_id,
                ..Default::default()
            },
            &ids,
        )
        .unwrap();
    assert_eq!(second.records.len(), 30);
    assert_eq!(second.next_before_id, None);
    assert!(
        first.records.last().unwrap()["id"].as_i64().unwrap()
            > second.records[0]["id"].as_i64().unwrap()
    );
    assert!(
        reader
            .list_for_tools(&RecordQuery::default(), &["tool' OR 1=1 --".into()])
            .unwrap()
            .records
            .is_empty()
    );
    for invalid in [
        vec![],
        vec![String::new()],
        vec!["x".repeat(1025)],
        vec!["tool".into(); 129],
    ] {
        assert!(matches!(
            reader.list_for_tools(&RecordQuery::default(), &invalid),
            Err(ReadError::InvalidQuery(_))
        ));
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn filters_and_deep_pages_use_writer_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    drop(StatsRecorder::new(&path).unwrap());
    let reader = StatsReader::open(&path).unwrap();
    let cases = [
        (
            RecordQuery {
                session_id: Some("s".into()),
                ..Default::default()
            },
            None,
            "idx_session",
        ),
        (
            RecordQuery {
                tool_use_id: Some("t".into()),
                ..Default::default()
            },
            None,
            "idx_tool_use_id",
        ),
        (
            RecordQuery {
                before_id: Some(1000),
                ..Default::default()
            },
            None,
            "INTEGER PRIMARY KEY",
        ),
        (
            RecordQuery::default(),
            Some(vec!["missing".into(), "missing".into()]),
            "idx_tool_use_id",
        ),
    ];
    for (query, ids, index) in cases {
        let (sql, values) = reader.list_sql(&query, ids.as_deref());
        let plan = reader
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(params_from_iter(values), |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains(index), "{plan}");
        assert!(!plan.contains("SCAN stats"), "{plan}");
    }
}

#[test]
fn writer_upgrades_tool_index_while_reader_preserves_legacy_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let conn = Connection::open(&path).unwrap();
    legacy(&conn);
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let reader = StatsReader::open(&path).unwrap();
    assert_eq!(
        reader
            .list_for_tools(&RecordQuery::default(), &["t".into()])
            .unwrap()
            .records
            .len(),
        1
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(reader);
    drop(StatsRecorder::new(&path).unwrap());
    let reader = StatsReader::open(&path).unwrap();
    let indexed: bool = reader
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_index_list('stats') WHERE name='idx_tool_use_id')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(indexed);
    assert_eq!(
        reader
            .list_for_tools(&RecordQuery::default(), &["t".into()])
            .unwrap()
            .records
            .len(),
        1
    );
}

#[test]
fn invalid_records_identify_only_the_source_id_and_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let conn = Connection::open(&path).unwrap();
    legacy(&conn);
    let reader = StatsReader::open(&path).unwrap();
    for (sql, field) in [
        (
            "UPDATE stats SET timestamp='private malformed timestamp'",
            "timestamp",
        ),
        (
            "UPDATE stats SET timestamp='2026-01-01T00:00:00Z',before_tokens=-1",
            "before_tokens",
        ),
        (
            "UPDATE stats SET before_tokens=1,operation=CAST(x'ff' AS TEXT)",
            "operation",
        ),
    ] {
        conn.execute(sql, []).unwrap();
        let error = reader.list(&RecordQuery::default()).unwrap_err();
        assert!(
            matches!(&error, ReadError::InvalidRecord { id: Some(1), field: actual, .. } if *actual == field)
        );
        assert!(!error.to_string().contains("private malformed"));
    }
}

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::{
    hermes, hermes_convert, scan_once, CollectorConfig, TrajectoryMaintenancePolicy,
    TrajectoryStore,
};

const SESSION_ID: &str = "agent-run-session-0001";

fn stored_id(path: &Path, original_id: &str) -> String {
    let database = if path.file_name().is_some_and(|name| name == "state.db") {
        path.to_path_buf()
    } else {
        path.join("state.db")
    }
    .canonicalize()
    .unwrap();
    hermes_convert::scoped_session_id(&hermes::database_namespace(&database), original_id)
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "agentsight-hermes-{tag}-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_hermes_db(home: &Path) -> Connection {
    let path = home.join("state.db");
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES (15);
             CREATE TABLE sessions (
                 id TEXT PRIMARY KEY,
                 source TEXT NOT NULL,
                 model TEXT,
                 parent_session_id TEXT,
                 cwd TEXT,
                 started_at REAL NOT NULL,
                 ended_at REAL,
                 input_tokens INTEGER DEFAULT 0,
                 output_tokens INTEGER DEFAULT 0,
                 cache_read_tokens INTEGER DEFAULT 0,
                 cache_write_tokens INTEGER DEFAULT 0,
                 reasoning_tokens INTEGER DEFAULT 0,
                 estimated_cost_usd REAL,
                 actual_cost_usd REAL,
                 api_call_count INTEGER DEFAULT 0
             );
             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT,
                 tool_call_id TEXT,
                 tool_calls TEXT,
                 tool_name TEXT,
                 timestamp REAL NOT NULL,
                 finish_reason TEXT,
                 reasoning TEXT,
                 reasoning_content TEXT,
                 active INTEGER NOT NULL DEFAULT 1
             );",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions (
                 id, source, model, cwd, started_at, input_tokens, output_tokens,
                 cache_read_tokens, cache_write_tokens, reasoning_tokens,
                 estimated_cost_usd, api_call_count
             ) VALUES (?1, 'api_server', 'glm-5.1', '/workspace/profiling',
                       1781517600.0, 120, 30, 20, 5, 7, 0.25, 2)",
            params![SESSION_ID],
        )
        .unwrap();
    let rows = [
        (
            "user",
            Some("Diagnose the GPU hang"),
            None,
            None,
            None,
            1781517600.0,
            None,
            None,
            1,
        ),
        (
            "assistant",
            Some("I will inspect the job"),
            None,
            Some(r#"[{"name":"sysomctl","arguments":"{\"task\":\"gpu\"}"}]"#),
            None,
            1781517601.0,
            Some("stop"),
            Some("Need runtime evidence"),
            1,
        ),
        (
            "tool",
            Some("diagnosis complete"),
            Some("call-sysom-1"),
            None,
            Some("sysomctl"),
            1781517602.0,
            None,
            None,
            1,
        ),
        (
            "assistant",
            Some("The job is blocked on a collective operation"),
            None,
            None,
            None,
            1781517603.0,
            Some("stop"),
            None,
            1,
        ),
        (
            "assistant",
            Some("rewound response"),
            None,
            None,
            None,
            1781517604.0,
            None,
            None,
            0,
        ),
    ];
    for (role, content, call_id, calls, tool_name, timestamp, finish, reasoning, active) in rows {
        connection
            .execute(
                "INSERT INTO messages (
                     session_id, role, content, tool_call_id, tool_calls, tool_name,
                     timestamp, finish_reason, reasoning_content, active
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    SESSION_ID, role, content, call_id, calls, tool_name, timestamp, finish,
                    reasoning, active
                ],
            )
            .unwrap();
    }
    connection
}

#[test]
fn scan_once_converts_live_hermes_wal_to_atif() {
    let base = tmp_dir("convert");
    let hermes_home = base.join("hermes-home");
    std::fs::create_dir_all(&hermes_home).unwrap();
    let writer = create_hermes_db(&hermes_home);
    let output = base.join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![hermes_home]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };

    scan_once(&store, &config);
    let record = store
        .get(&stored_id(
            &config.scan_dirs.as_ref().unwrap()[0],
            SESSION_ID,
        ))
        .unwrap()
        .unwrap();
    assert_eq!(record.source, "hermes");
    let document: serde_json::Value = serde_json::from_str(&record.atif_json).unwrap();
    assert_eq!(document["session_id"], record.session_id);
    assert_eq!(document["extra"]["hermes_session_id"], SESSION_ID);
    assert_eq!(record.agent_name, "hermes");
    assert_eq!(record.model_name.as_deref(), Some("glm-5.1"));
    assert_eq!(record.project, "profiling");
    assert_eq!(record.total_prompt_tokens, Some(120));
    assert_eq!(record.total_completion_tokens, Some(30));

    let document: serde_json::Value = serde_json::from_str(&record.atif_json).unwrap();
    let steps = document["steps"].as_array().unwrap();
    assert_eq!(
        steps.len(),
        3,
        "tool results attach to the calling agent step"
    );
    assert_eq!(steps[0]["source"], "user");
    assert_eq!(steps[1]["reasoning_content"], "Need runtime evidence");
    assert_eq!(steps[1]["tool_calls"][0]["tool_call_id"], "call-sysom-1");
    assert_eq!(steps[1]["tool_calls"][0]["function_name"], "sysomctl");
    assert_eq!(
        steps[1]["observation"]["results"][0]["content"],
        "diagnosis complete"
    );
    assert_eq!(document["final_metrics"]["total_cached_tokens"], 20);
    assert_eq!(document["final_metrics"]["total_steps"], 3);
    assert_eq!(
        document["agent"]["extra"]["hermes_state_schema_version"],
        15
    );
    assert!(
        !record.atif_json.contains("rewound response"),
        "inactive Hermes messages must not reappear in trajectories"
    );

    let first_collected_at: i64 = Connection::open(&output)
        .unwrap()
        .query_row(
            "SELECT collected_at_ns FROM collected_trajectories WHERE session_id = ?1",
            params![stored_id(
                &config.scan_dirs.as_ref().unwrap()[0],
                SESSION_ID
            )],
            |row| row.get(0),
        )
        .unwrap();
    scan_once(&store, &config);
    let second_collected_at: i64 = Connection::open(&output)
        .unwrap()
        .query_row(
            "SELECT collected_at_ns FROM collected_trajectories WHERE session_id = ?1",
            params![stored_id(
                &config.scan_dirs.as_ref().unwrap()[0],
                SESSION_ID
            )],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first_collected_at, second_collected_at);

    writer
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp) VALUES (?1, 'user', 'continue', 1781517605.0)",
            params![SESSION_ID],
        )
        .unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store
            .get(&stored_id(
                &config.scan_dirs.as_ref().unwrap()[0],
                SESSION_ID
            ))
            .unwrap()
            .unwrap()
            .num_steps,
        4
    );
}

#[test]
fn scan_once_accepts_database_path_without_jsonl_root() {
    let base = tmp_dir("database-path");
    let hermes_home = base.join("hermes-home");
    std::fs::create_dir_all(&hermes_home).unwrap();
    let _writer = create_hermes_db(&hermes_home);
    let database = hermes_home.join("state.db");

    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![database]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };
    scan_once(&store, &config);
    assert_eq!(
        store
            .get(&stored_id(
                &config.scan_dirs.as_ref().unwrap()[0],
                SESSION_ID
            ))
            .unwrap()
            .unwrap()
            .num_steps,
        3
    );
}

#[test]
fn scan_once_skips_malformed_session_and_keeps_healthy_sessions() {
    let base = tmp_dir("malformed-session");
    let hermes_home = base.join("hermes-home");
    std::fs::create_dir_all(&hermes_home).unwrap();
    let writer = create_hermes_db(&hermes_home);
    // 坏会话排在健康会话之前，错误直接传播时健康会话将无法入库。
    writer
        .execute_batch(
            "INSERT INTO sessions (id, source, started_at) VALUES ('broken-session', 'cli', 1.0);
             INSERT INTO messages (session_id, role, content, timestamp)
             VALUES ('broken-session', X'00', 'unreadable role', 1.0);",
        )
        .unwrap();
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![hermes_home]),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: 0,
            ..TrajectoryMaintenancePolicy::default()
        },
    };

    scan_once(&store, &config);
    assert!(store.get("broken-session").unwrap().is_none());
    assert_eq!(
        store
            .get(&stored_id(
                &config.scan_dirs.as_ref().unwrap()[0],
                SESSION_ID
            ))
            .unwrap()
            .unwrap()
            .num_steps,
        3
    );
}

#[test]
fn scoped_identity_is_version_stable_and_supports_arbitrary_ids() {
    assert_eq!(
        hermes_convert::scoped_session_id("test-database", "自定义/id:001"),
        "hermes:11b9db88584f303988c3f62e3b1a995342c9b24d920a9f1d1e297152a68a7d96"
    );
    assert_ne!(
        hermes_convert::scoped_session_id("test-database", "custom"),
        hermes_convert::scoped_session_id("test-database", "custom:1")
    );
}

#[test]
fn session_row_decode_errors_do_not_block_healthy_rows() {
    let base = tmp_dir("session-rows");
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let writer = create_hermes_db(&home);
    writer
        .execute_batch(
            "INSERT INTO sessions (id, source, started_at) VALUES (NULL, 'cli', 1.0);
         INSERT INTO sessions (id, source, started_at) VALUES (X'00', 'cli', 2.0);
         INSERT INTO sessions (id, source, started_at) VALUES ('bad-source', X'00', 3.0);
         INSERT INTO sessions (id, source, started_at) VALUES ('', 'cli', 4.0);",
        )
        .unwrap();
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    let outcome = hermes::collect_database(&store, &home.join("state.db"), 0).unwrap();
    assert_eq!((outcome.sessions, outcome.ingested), (1, 1));
    assert!(store.get(&stored_id(&home, SESSION_ID)).unwrap().is_some());
    assert_eq!(
        hermes::collect_database(&store, &home.join("state.db"), 0)
            .unwrap()
            .ingested,
        0
    );
}

#[test]
fn session_state_query_does_not_read_atif_and_checks_source_identity() {
    let base = tmp_dir("scalar-state");
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _writer = create_hermes_db(&home);
    let output = base.join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    hermes::collect_database(&store, &home.join("state.db"), 0).unwrap();
    let id = stored_id(&home, SESSION_ID);
    let path = home
        .join("state.db")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let state = store
        .get_source_session_state(&id, "hermes", &path)
        .unwrap();
    assert!(state.is_some());
    assert!(store
        .get_source_session_state(&id, "claude-code", &path)
        .unwrap()
        .is_none());
    assert!(store
        .get_source_session_state(&id, "hermes", "other.db")
        .unwrap()
        .is_none());
    assert!(store
        .get_source_session_state("missing", "hermes", &path)
        .unwrap()
        .is_none());
    // 详情列无法解码时状态查询仍成功，证明查询没有投影正文列。
    Connection::open(&output)
        .unwrap()
        .execute(
            "UPDATE collected_trajectories SET atif_json = X'00' WHERE session_id = ?1",
            params![id],
        )
        .unwrap();
    assert!(store.get(&id).is_err());
    assert_eq!(
        store
            .get_source_session_state(&id, "hermes", &path)
            .unwrap(),
        state
    );
    assert_eq!(
        hermes::collect_database(&store, &home.join("state.db"), 0)
            .unwrap()
            .ingested,
        0
    );
}

#[test]
fn identical_ids_in_two_databases_are_independent_across_scans_and_restart() {
    let base = tmp_dir("two-databases");
    let left = base.join("left");
    let right = base.join("right");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    let _left_writer = create_hermes_db(&left);
    let right_writer = create_hermes_db(&right);
    right_writer
        .execute("UPDATE messages SET content = 'right database'", [])
        .unwrap();
    let output = base.join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    let mut config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![left.clone(), right.clone()]),
        maintenance: TrajectoryMaintenancePolicy {
            retention_days: 0,
            ..TrajectoryMaintenancePolicy::default()
        },
    };
    scan_once(&store, &config);
    let left_id = stored_id(&left, SESSION_ID);
    let right_id = stored_id(&right, SESSION_ID);
    assert_ne!(left_id, right_id);
    assert_eq!(store.count().unwrap(), 2);
    let left_payload = store.get_atif_json(&left_id).unwrap().unwrap();
    let right_payload = store.get_atif_json(&right_id).unwrap().unwrap();
    assert_ne!(left_payload, right_payload);
    for (id, payload) in [(&left_id, &left_payload), (&right_id, &right_payload)] {
        let doc: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(doc["session_id"], *id);
        assert_eq!(doc["extra"]["hermes_session_id"], SESSION_ID);
    }
    let timestamps = || {
        let connection = Connection::open(&output).unwrap();
        let mut stmt = connection.prepare("SELECT session_id, collected_at_ns FROM collected_trajectories ORDER BY session_id").unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    };
    let before = timestamps();
    config.scan_dirs.as_mut().unwrap().reverse();
    scan_once(&store, &config);
    assert_eq!(timestamps(), before);
    drop(store);
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    scan_once(&store, &config);
    assert_eq!(timestamps(), before);
    right_writer.execute(
        "INSERT INTO messages (session_id, role, content, timestamp) VALUES (?1, 'user', 'continue right', 1781517610.0)",
        params![SESSION_ID],
    ).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get_atif_json(&left_id).unwrap().unwrap(),
        left_payload
    );
    assert_eq!(store.get(&right_id).unwrap().unwrap().num_steps, 4);
}

#[test]
fn parent_reference_uses_same_database_namespace() {
    let base = tmp_dir("parent-id");
    let _writer = create_hermes_db(&base);
    _writer
        .execute("UPDATE sessions SET parent_session_id = 'parent-001'", [])
        .unwrap();
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    hermes::collect_database(&store, &base.join("state.db"), 0).unwrap();
    let payload = store
        .get_atif_json(&stored_id(&base, SESSION_ID))
        .unwrap()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        doc["extra"]["parent_session_id"],
        stored_id(&base, "parent-001")
    );
    assert_eq!(doc["extra"]["hermes_parent_session_id"], "parent-001");
}

#[cfg(unix)]
#[test]
fn database_symlink_aliases_share_one_identity() {
    let base = tmp_dir("alias-id");
    let real = base.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let _writer = create_hermes_db(&real);
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    assert_eq!(stored_id(&real, SESSION_ID), stored_id(&alias, SESSION_ID));
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    hermes::collect_database(&store, &real.join("state.db"), 0).unwrap();
    assert_eq!(
        hermes::collect_database(&store, &alias.join("state.db"), 0)
            .unwrap()
            .ingested,
        0
    );
    assert_eq!(store.count().unwrap(), 1);
}

#[test]
fn capacity_eviction_does_not_reingest_unchanged_hermes_session() {
    let base = tmp_dir("evicted-state");
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let writer = create_hermes_db(&home);
    // 单条约 1.2 MiB 轨迹超过 1 MiB 测试限额，稳定触发真实容量裁剪。
    writer
        .execute(
            "UPDATE messages SET content = ?1 WHERE role = 'user'",
            params!["x".repeat(1_250_000)],
        )
        .unwrap();
    let output = base.join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    let database = home.join("state.db");
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        1
    );
    let report = store
        .maintain(TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .unwrap();
    assert!(report.size.deleted_rows > 0);
    assert_eq!(store.count().unwrap(), 0);
    for _ in 0..2 {
        assert_eq!(
            hermes::collect_database(&store, &database, 0)
                .unwrap()
                .ingested,
            0,
            "被裁剪且源未变化的会话不能重新入库"
        );
        store
            .maintain(TrajectoryMaintenancePolicy {
                retention_days: 0,
                max_db_size_mb: 1,
            })
            .unwrap();
    }
    drop(store);
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        0
    );
    writer.execute("INSERT INTO messages (session_id, role, content, timestamp) VALUES (?1, 'user', 'new turn', 1781517620.0)", params![SESSION_ID]).unwrap();
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        1
    );
    assert_eq!(
        store
            .get(&stored_id(&home, SESSION_ID))
            .unwrap()
            .unwrap()
            .num_steps,
        4
    );
}

#[test]
fn evicted_states_are_independent_for_sessions_in_one_database() {
    let base = tmp_dir("evicted-multiple");
    let writer = create_hermes_db(&base);
    writer.execute_batch(
        "INSERT INTO sessions (id, source, started_at) VALUES ('second-session', 'cli', 1781517600.0);
         INSERT INTO messages (session_id, role, content, timestamp) VALUES ('second-session', 'user', 'second', 1781517601.0);"
    ).unwrap();
    writer
        .execute(
            "UPDATE messages SET content = ?1 WHERE role = 'user'",
            params!["x".repeat(850_000)],
        )
        .unwrap();
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    let database = base.join("state.db");
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        2
    );
    let report = store
        .maintain(TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .unwrap();
    assert!(report.size.deleted_rows > 0);
    let remaining = store.count().unwrap();
    assert!(remaining < 2);
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        0
    );
    assert_eq!(store.count().unwrap(), remaining);
    let path = database
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    for id in [SESSION_ID, "second-session"] {
        assert!(store
            .get_source_session_state(&stored_id(&base, id), "hermes", &path)
            .unwrap()
            .is_some());
    }
    writer.execute("INSERT INTO messages (session_id, role, content, timestamp) VALUES ('second-session', 'user', 'new', 1781517620.0)", []).unwrap();
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        1
    );
    assert_eq!(
        store
            .get(&stored_id(&base, "second-session"))
            .unwrap()
            .unwrap()
            .num_steps,
        2
    );
}

#[test]
fn session_state_retention_can_widen_and_missing_source_is_cleaned() {
    let base = tmp_dir("state-lifecycle");
    let writer = create_hermes_db(&base);
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    let database = base.join("state.db");
    hermes::collect_database(&store, &database, 0).unwrap();
    Connection::open(base.join("trajectories.db"))
        .unwrap()
        .execute("UPDATE collected_trajectories SET collected_at_ns = 1", [])
        .unwrap();
    store
        .maintain(TrajectoryMaintenancePolicy {
            retention_days: 1,
            max_db_size_mb: 0,
        })
        .unwrap();
    assert_eq!(store.count().unwrap(), 0);
    assert_eq!(
        hermes::collect_database(&store, &database, 1)
            .unwrap()
            .ingested,
        0
    );
    assert_eq!(
        hermes::collect_database(&store, &database, 0)
            .unwrap()
            .ingested,
        1
    );
    let id = stored_id(&base, SESSION_ID);
    drop(writer);
    std::fs::rename(&database, base.join("archived.db")).unwrap();
    assert_eq!(store.remove_stale_source_session_states(None).unwrap(), 1);
    let conn = Connection::open(base.join("trajectories.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM source_session_states", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert!(
        store.get(&id).unwrap().is_some(),
        "清理扫描状态不删除已保存的正文"
    );
}

#[test]
fn older_database_backfills_session_state_without_reading_payload() {
    let base = tmp_dir("state-upgrade");
    let _writer = create_hermes_db(&base);
    let output = base.join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    hermes::collect_database(&store, &base.join("state.db"), 0).unwrap();
    let id = stored_id(&base, SESSION_ID);
    drop(store);
    let conn = Connection::open(&output).unwrap();
    conn.execute_batch("DROP TABLE source_session_states;")
        .unwrap();
    conn.execute(
        "UPDATE collected_trajectories SET atif_json = X'00' WHERE session_id = ?1",
        params![id],
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    conn.execute(
        "DELETE FROM collected_trajectories WHERE session_id = ?1",
        params![id],
    )
    .unwrap();
    assert_eq!(
        hermes::collect_database(&store, &base.join("state.db"), 0)
            .unwrap()
            .ingested,
        0
    );
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn source_state_write_failure_rolls_back_payload() {
    let base = tmp_dir("state-atomicity");
    let _writer = create_hermes_db(&base);
    let output = base.join("trajectories.db");
    let store = TrajectoryStore::new_with_path(&output).unwrap();
    Connection::open(&output)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_source_state BEFORE INSERT ON source_session_states
         BEGIN SELECT RAISE(ABORT, 'test write failure'); END;",
        )
        .unwrap();
    assert_eq!(
        hermes::collect_database(&store, &base.join("state.db"), 0)
            .unwrap()
            .ingested,
        0
    );
    assert_eq!(store.count().unwrap(), 0);
    let id = stored_id(&base, SESSION_ID);
    let path = base
        .join("state.db")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(store
        .get_source_session_state(&id, "hermes", &path)
        .unwrap()
        .is_none());
}

#[test]
fn scan_once_rejects_non_hermes_sqlite_without_affecting_other_sources() {
    let base = tmp_dir("wrong-schema");
    let foreign_home = base.join("foreign");
    std::fs::create_dir_all(&foreign_home).unwrap();
    Connection::open(foreign_home.join("state.db"))
        .unwrap()
        .execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);")
        .unwrap();
    let store = TrajectoryStore::new_with_path(&base.join("trajectories.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![foreign_home]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };

    scan_once(&store, &config);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn hermes_parallel_results_keep_explicit_call_ids() {
    assert_hermes_result_identity(false);
}

#[test]
fn hermes_late_results_find_the_earlier_exact_call() {
    assert_hermes_result_identity(true);
}

fn assert_hermes_result_identity(separate_steps: bool) {
    let base = tmp_dir(if separate_steps {
        "late-results"
    } else {
        "parallel-results"
    });
    let writer = create_hermes_db(&base);
    writer.execute("DELETE FROM messages", []).unwrap();
    let calls = if separate_steps {
        vec![
            r#"[{"id":"call-a","function":{"name":"read_file","arguments":{"path":"a.txt"}}}]"#,
            r#"[{"id":"call-b","function":{"name":"read_file","arguments":{"path":"b.txt"}}}]"#,
        ]
    } else {
        vec![
            r#"[{"id":"call-a","function":{"name":"read_file","arguments":{"path":"a.txt"}}},{"id":"call-b","function":{"name":"read_file","arguments":{"path":"b.txt"}}}]"#,
        ]
    };
    for tool_calls in calls {
        writer.execute(
            "INSERT INTO messages (session_id, role, tool_calls, timestamp) VALUES (?1, 'assistant', ?2, 1781517601)",
            params![SESSION_ID, tool_calls],
        ).unwrap();
    }
    // Reverse result order exercises both exact call lookup and repeated tool names.
    for (id, content) in [("call-b", "contents of b"), ("call-a", "contents of a")] {
        writer.execute(
            "INSERT INTO messages (session_id, role, tool_call_id, tool_name, content, timestamp) VALUES (?1, 'tool', ?2, 'read_file', ?3, 1781517602)",
            params![SESSION_ID, id, content],
        ).unwrap();
    }
    let store = TrajectoryStore::new_with_path(&base.join("output.db")).unwrap();
    let outcome = hermes::collect_database(&store, &base.join("state.db"), 0).unwrap();
    assert_eq!(outcome.ingested, 1);
    let record = store.get(&stored_id(&base, SESSION_ID)).unwrap().unwrap();
    let document: serde_json::Value = serde_json::from_str(&record.atif_json).unwrap();
    let steps = document["steps"].as_array().unwrap();
    let mut observed = std::collections::HashSet::new();
    for step in steps {
        for call in step["tool_calls"].as_array().unwrap() {
            let id = call["tool_call_id"].as_str().unwrap();
            assert!(observed.insert(id.to_string()), "call IDs must stay unique");
            let path = call["arguments"]["path"].as_str().unwrap();
            assert_eq!(id, if path == "a.txt" { "call-a" } else { "call-b" });
            assert_eq!(
                call["extra"]["result_timestamp"],
                "2026-06-15T10:00:02.000Z"
            );
            let results = step["observation"]["results"].as_array().unwrap();
            let result = results
                .iter()
                .find(|result| result["source_call_id"] == id)
                .unwrap();
            assert_eq!(
                result["content"],
                if id == "call-a" {
                    "contents of a"
                } else {
                    "contents of b"
                }
            );
        }
    }
    assert_eq!(observed.len(), 2);
    assert_eq!(
        hermes::collect_database(&store, &base.join("state.db"), 0)
            .unwrap()
            .ingested,
        0
    );
    drop(store);
    drop(writer);
    std::fs::remove_dir_all(base).unwrap();
}

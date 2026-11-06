use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use rusqlite::Connection;

struct Fixture {
    _dir: tempfile::TempDir,
    writer: Connection,
    output: Connection,
    store: TrajectoryStore,
    config: CollectorConfig,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let writer = Connection::open(dir.path().join("state.db")).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
            CREATE TABLE sessions (id TEXT PRIMARY KEY);
            INSERT INTO sessions VALUES ('rewind'),('keep');
            CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT,
                role TEXT, content TEXT, timestamp REAL, active INTEGER DEFAULT 1);
            INSERT INTO messages (session_id,role,content,timestamp) VALUES
                ('rewind','user','Withdraw this question',1),
                ('rewind','assistant','Withdraw this answer',2),
                ('keep','user','Keep this other session',3);",
            )
            .unwrap();
        let path = dir.path().join("output.db");
        let store = TrajectoryStore::new_with_path(&path).unwrap();
        let output = Connection::open(path).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![dir.path().join("state.db")]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };
        let f = Self {
            _dir: dir,
            writer,
            output,
            store,
            config,
        };
        f.scan();
        assert_eq!(f.store.count().unwrap(), 2);
        f
    }

    fn scan(&self) {
        scan_once(&self.store, &self.config);
    }

    fn rewind_id(&self) -> String {
        self.output
            .query_row(
                "SELECT session_id FROM collected_trajectories
            WHERE first_user_message = 'Withdraw this question'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
}

#[test]
fn empty_active_history_removes_only_that_session_and_can_repopulate() {
    let f = Fixture::new();
    let id = f.rewind_id();
    f.writer
        .execute("UPDATE messages SET active=0 WHERE session_id='rewind'", [])
        .unwrap();
    f.scan();
    assert!(
        f.store.get(&id).unwrap().is_none(),
        "withdrawn content must leave the viewer"
    );
    assert_eq!(
        f.store.count().unwrap(),
        1,
        "another session in the same file must survive"
    );
    let states: i64 = f
        .output
        .query_row(
            "SELECT COUNT(*) FROM source_session_states WHERE session_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        states, 0,
        "the old fingerprint must not block restoring the same history"
    );
    f.scan();
    assert_eq!(f.store.count().unwrap(), 1);
    f.writer
        .execute("UPDATE messages SET active=1 WHERE session_id='rewind'", [])
        .unwrap();
    f.scan();
    assert_eq!(f.store.get(&id).unwrap().unwrap().num_steps, 2);
}

#[test]
fn failed_read_preserves_last_successful_snapshot() {
    let f = Fixture::new();
    let id = f.rewind_id();
    f.writer
        .execute(
            "UPDATE messages SET role=NULL WHERE session_id='rewind'",
            [],
        )
        .unwrap();
    f.scan();
    assert_eq!(f.store.get(&id).unwrap().unwrap().num_steps, 2);
    assert_eq!(f.store.count().unwrap(), 2);
}

#[test]
fn deleting_all_messages_clears_payload_and_fingerprint_together() {
    let f = Fixture::new();
    let id = f.rewind_id();
    f.writer
        .execute("DELETE FROM messages WHERE session_id='rewind'", [])
        .unwrap();
    f.output
        .execute_batch(
            "CREATE TRIGGER reject_state_delete BEFORE DELETE ON source_session_states
        BEGIN SELECT RAISE(ABORT,'injected storage failure'); END;",
        )
        .unwrap();
    f.scan();
    assert!(
        f.store.get(&id).unwrap().is_some(),
        "failed cleanup must roll back payload deletion"
    );
    f.output
        .execute_batch("DROP TRIGGER reject_state_delete;")
        .unwrap();
    f.scan();
    assert!(f.store.get(&id).unwrap().is_none());
    assert_eq!(f.store.count().unwrap(), 1);
}

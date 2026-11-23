//! Discovery metadata changes must invalidate the collected Gemini snapshot.
use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::json;

#[test]
fn restored_project_marker_refreshes_an_unchanged_conversation() {
    let root = tempfile::tempdir().unwrap();
    let scan = root.path().join(".gemini/tmp");
    let project = scan.join("opaque-project-hash");
    let chats = project.join("chats");
    std::fs::create_dir_all(&chats).unwrap();
    let file = chats.join("session-fixture.jsonl");
    let events = [
        json!({"sessionId":"fixture","projectHash":"opaque-project-hash"}),
        json!({"id":"u","type":"user","content":"Inspect project"}),
        json!({"id":"a","type":"gemini","content":"Done"}),
    ];
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    std::fs::write(&file, &content).unwrap();
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    let store = TrajectoryStore::new_with_path(&root.path().join("collection.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![scan]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };
    scan_once(&store, &config);
    assert_eq!(
        store.get("session-fixture").unwrap().unwrap().project,
        "opaque-project-hash"
    );
    std::fs::write(
        project.join(".project_root"),
        "/workspace/restored-project\n",
    )
    .unwrap();
    scan_once(&store, &config);
    let row = store.get("session-fixture").unwrap().unwrap();
    assert_eq!(row.project, "restored-project");
    let atif: serde_json::Value = serde_json::from_str(&row.atif_json).unwrap();
    assert_eq!(atif["extra"]["project"], "restored-project");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(store.count().unwrap(), 1);
    let state = store.get_file_state(&file.to_string_lossy()).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get_file_state(&file.to_string_lossy()).unwrap(),
        state
    );

    // Metadata changes apply to historical chats without rewriting them.
    std::fs::write(project.join(".project_root"), "/workspace/renamed-project").unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("session-fixture").unwrap().unwrap().project,
        "renamed-project"
    );
    let changed = store.get_file_state(&file.to_string_lossy()).unwrap();
    assert_ne!(changed, state);
    std::fs::remove_file(project.join(".project_root")).unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store.get("session-fixture").unwrap().unwrap().project,
        "opaque-project-hash"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(store.count().unwrap(), 1);

    // A content update must still invalidate the same metadata-aware cache.
    std::fs::write(
        &file,
        format!(
            "{content}{}\n",
            json!({"id":"u2","type":"user","content":"New request"})
        ),
    )
    .unwrap();
    scan_once(&store, &config);
    assert_eq!(
        store
            .get("session-fixture")
            .unwrap()
            .unwrap()
            .last_user_message
            .as_deref(),
        Some("New request")
    );
}

#[test]
fn unchanged_project_marker_does_not_rewrite_the_collected_row() {
    let root = tempfile::tempdir().unwrap();
    let scan = root.path().join(".gemini/tmp");
    let project = scan.join("project");
    let chats = project.join("chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(project.join(".project_root"), "/workspace/same-project").unwrap();
    std::fs::write(
        chats.join("session-stable.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"sessionId":"stable","projectHash":"project"}),
            json!({"id":"u","type":"user","content":"Stable request"})
        ),
    )
    .unwrap();
    let database = root.path().join("collection.db");
    let store = TrajectoryStore::new_with_path(&database).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![scan]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };
    scan_once(&store, &config);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.execute_batch("CREATE TABLE rewrite_count (n INTEGER); INSERT INTO rewrite_count VALUES (0); CREATE TRIGGER count_rewrite AFTER UPDATE ON collected_trajectories BEGIN UPDATE rewrite_count SET n=n+1; END;").unwrap();
    // Rewriting the same marker must not cause expensive trajectory rescans.
    std::fs::write(project.join(".project_root"), "/workspace/same-project\n").unwrap();
    scan_once(&store, &config);
    assert_eq!(
        connection
            .query_row("SELECT n FROM rewrite_count", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store.get("session-stable").unwrap().unwrap().project,
        "same-project"
    );
}

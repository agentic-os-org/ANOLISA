use agentsight_trajectory_collector::{
    CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
};
use serde_json::json;
use std::{fs, io::Write, path::PathBuf};

struct Fixture {
    root: PathBuf,
    file: PathBuf,
    store: TrajectoryStore,
    config: CollectorConfig,
}
const ID: &str = "session-2026-10-27T09-00-ab12cd34";
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("gemini-empty-{}-{nonce}", std::process::id()));
        let scan = root.join(".gemini/tmp");
        let chats = scan.join("project/chats");
        fs::create_dir_all(&chats).unwrap();
        let file = chats.join(format!("{ID}.jsonl"));
        let events = [
            json!({"sessionId":"fixture","projectHash":"project"}),
            json!({"id":"u","type":"user","content":"old request"}),
            json!({"id":"a","type":"gemini","content":"old answer","tokens":{"input":10,"output":5}}),
        ];
        fs::write(
            &file,
            events.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
        let store = TrajectoryStore::new_with_path(&root.join("collected.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![scan]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };
        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);
        Self {
            root,
            file,
            store,
            config,
        }
    }
    fn append(&self, event: serde_json::Value) {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&self.file)
                .unwrap(),
            "{event}"
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn rewinding_all_messages_removes_old_content_and_can_resume() {
    let fixture = Fixture::new();
    fixture.append(json!({"$rewindTo":"u"}));
    scan_once(&fixture.store, &fixture.config);
    assert_eq!(
        fixture.store.count().unwrap(),
        0,
        "the previous snapshot is no longer the conversation"
    );
    assert!(fixture.store.get_atif_json(ID).unwrap().is_none());
    let state = fixture
        .store
        .get_file_state(&fixture.file.to_string_lossy())
        .unwrap();
    assert!(
        state.is_some(),
        "remember a successfully replayed empty file"
    );
    scan_once(&fixture.store, &fixture.config);
    assert_eq!(
        fixture
            .store
            .get_file_state(&fixture.file.to_string_lossy())
            .unwrap(),
        state
    );
    fixture.append(json!({"id":"new-u","type":"user","content":"new request"}));
    scan_once(&fixture.store, &fixture.config);
    let record = fixture.store.get(ID).unwrap().unwrap();
    assert_eq!(record.first_user_message.as_deref(), Some("new request"));
    assert!(!record.atif_json.contains("old answer"));
}

#[test]
fn replacing_messages_with_an_empty_snapshot_clears_previews() {
    let fixture = Fixture::new();
    fixture.append(json!({"$set":{"messages":[]}}));
    scan_once(&fixture.store, &fixture.config);
    assert!(fixture.store.get(ID).unwrap().is_none());
    assert!(
        fixture
            .store
            .list_recent_atif_jsons(0, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn broken_files_do_not_erase_the_last_successful_snapshot() {
    let fixture = Fixture::new();
    fs::write(&fixture.file, "not valid JSON\n").unwrap();
    scan_once(&fixture.store, &fixture.config);
    assert_eq!(
        fixture
            .store
            .get(ID)
            .unwrap()
            .unwrap()
            .first_user_message
            .as_deref(),
        Some("old request")
    );
}

#[test]
fn a_partial_rewind_retains_the_remaining_prefix() {
    let fixture = Fixture::new();
    fixture.append(json!({"$rewindTo":"a"}));
    scan_once(&fixture.store, &fixture.config);
    let record = fixture.store.get(ID).unwrap().unwrap();
    assert_eq!(record.num_steps, 1);
    assert_eq!(record.first_user_message.as_deref(), Some("old request"));
    assert!(!record.atif_json.contains("old answer"));
}

#[test]
fn incomplete_metadata_only_rewrite_keeps_the_last_snapshot() {
    let fixture = Fixture::new();
    fs::write(
        &fixture.file,
        "{\"sessionId\":\"fixture\",\"projectHash\":\"project\"}\n{\"id\":",
    )
    .unwrap();
    scan_once(&fixture.store, &fixture.config);
    assert_eq!(fixture.store.get(ID).unwrap().unwrap().num_steps, 2);
}

#[test]
fn failed_bookkeeping_rolls_back_the_snapshot_removal() {
    let fixture = Fixture::new();
    let connection = rusqlite::Connection::open(fixture.root.join("collected.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_clear BEFORE INSERT ON skipped_files BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
    fixture.append(json!({"$rewindTo":"u"}));
    scan_once(&fixture.store, &fixture.config);
    assert!(fixture.store.get(ID).unwrap().is_some());
    connection
        .execute_batch("DROP TRIGGER reject_clear")
        .unwrap();
    scan_once(&fixture.store, &fixture.config);
    assert!(
        fixture.store.get(ID).unwrap().is_none(),
        "failed clear remains retryable"
    );
}

#[test]
fn clearing_a_copy_does_not_delete_another_paths_snapshot() {
    let fixture = Fixture::new();
    let mut record = fixture.store.get(ID).unwrap().unwrap();
    let other = fixture.root.join("archived-session.jsonl");
    fs::copy(&fixture.file, &other).unwrap();
    record.file_path = other.to_string_lossy().into_owned();
    fixture.store.upsert_trajectory(&record).unwrap();
    fixture.append(json!({"$rewindTo":"u"}));
    scan_once(&fixture.store, &fixture.config);
    assert_eq!(
        fixture.store.get(ID).unwrap().unwrap().file_path,
        record.file_path
    );
}

#[test]
fn malformed_replacement_is_not_an_explicit_empty_snapshot() {
    let fixture = Fixture::new();
    fixture.append(json!({"$set":{"messages":[null, false, {}]}}));
    scan_once(&fixture.store, &fixture.config);
    assert!(fixture.store.get(ID).unwrap().is_some());
}

#[test]
fn removing_all_ids_clears_the_stored_conversation() {
    let fixture = Fixture::new();
    fixture.append(json!({"$patch":{"removeIds":["u","a"]}}));
    scan_once(&fixture.store, &fixture.config);
    assert!(fixture.store.get(ID).unwrap().is_none());
}

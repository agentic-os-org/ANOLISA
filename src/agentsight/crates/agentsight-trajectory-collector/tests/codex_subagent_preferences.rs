//! Delegated Codex prompts remain retrievable without becoming user preference evidence.

use agentsight_trajectory_collector::{scan_once, CollectorConfig, TrajectoryStore};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture {
    root: PathBuf,
    sessions: PathBuf,
    store: TrajectoryStore,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("codex-preferences-{}-{nonce}", std::process::id()));
        let sessions = root.join(".codex/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let store = TrajectoryStore::new_with_path(&root.join("trajectories.db")).unwrap();
        Self {
            root,
            sessions,
            store,
        }
    }

    fn write(&self, id: &str, source: Option<Value>, prompt: &str) -> PathBuf {
        let mut meta = json!({"type":"session_meta","payload":{"id":id}});
        if let Some(source) = source {
            meta["payload"]["source"] = source;
        }
        // A source field outside session_meta must not classify a parent as a child.
        let user = json!({"type":"event_msg","payload":{"type":"user_message","message":prompt,"source":"subagent"}});
        let path = self.sessions.join(format!("{id}.jsonl"));
        std::fs::write(&path, format!("{meta}\n{user}\n")).unwrap();
        path
    }

    fn scan(&self) {
        scan_once(
            &self.store,
            &CollectorConfig {
                scan_interval_secs: 1,
                scan_dirs: Some(vec![self.sessions.clone()]),
                maintenance: Default::default(),
            },
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn thread_spawn() -> Option<Value> {
    Some(json!({"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}))
}

#[test]
fn only_parent_contributes_preference_evidence_across_scans() {
    let f = Fixture::new();
    f.write("parent", Some(json!("cli")), "Prefer short answers.");
    f.write("child", thread_spawn(), "Inspect the parser.");
    f.scan();
    let parent = f.store.get("parent").unwrap().unwrap();
    let child = f.store.get("child").unwrap().unwrap();
    assert!(!parent.is_subagent);
    assert!(child.is_subagent);
    assert_eq!(
        parent.first_user_message.as_deref(),
        Some("Prefer short answers.")
    );
    assert_eq!(
        child.first_user_message.as_deref(),
        Some("Inspect the parser.")
    );

    // A sentinel makes an unexpected rewrite observable independently of clock resolution.
    rusqlite::Connection::open(f.root.join("trajectories.db"))
        .unwrap()
        .execute("UPDATE collected_trajectories SET collected_at_ns = 1", [])
        .unwrap();
    let expected = vec![(parent.session_id, parent.atif_json)];
    assert_eq!(f.store.list_recent_atif_jsons(0, 50).unwrap(), expected);
    f.scan();
    let rows = f.store.list_summaries(None, None, None, 50).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.collected_at_ns == 1));
    assert_eq!(f.store.list_recent_atif_jsons(0, 50).unwrap(), expected);
}

#[test]
fn unchanged_legacy_child_is_reclassified_on_next_scan() {
    let f = Fixture::new();
    let path = f.write("child", thread_spawn(), "Inspect the parser.");
    f.scan();
    let mut legacy = f.store.get("child").unwrap().unwrap();
    legacy.is_subagent = false;
    legacy.file_size = std::fs::metadata(path).unwrap().len() as i64;
    // Upsert populates both the trajectory and its per-path scan bookkeeping.
    f.store.upsert_trajectory(&legacy).unwrap();
    assert_eq!(
        f.store.list_recent_atif_jsons(0, 50).unwrap(),
        vec![(legacy.session_id, legacy.atif_json)]
    );

    f.scan();
    let child = f.store.get("child").unwrap().unwrap();
    assert!(child.is_subagent);
    assert!(child.file_size < 0);
    assert_eq!(child.file_mtime_ns, legacy.file_mtime_ns);
    assert!(f.store.list_recent_atif_jsons(0, 50).unwrap().is_empty());
}

#[test]
fn only_active_session_meta_markers_identify_subagents() {
    let f = Fixture::new();
    let cases = [
        ("vscode", Some(json!("vscode")), false),
        ("exec", Some(json!("exec")), false),
        ("null", Some(Value::Null), false),
        ("missing", None, false),
        ("other-object", Some(json!({"origin":"cli"})), false),
        ("null-marker", Some(json!({"subagent":null})), false),
        ("false-marker", Some(json!({"subagent":false})), false),
        ("string-source", Some(json!("subagent")), true),
        ("string-marker", Some(json!({"subagent":"review"})), true),
        ("true-marker", Some(json!({"subagent":true})), true),
    ];
    for (id, source, _) in &cases {
        f.write(id, source.clone(), "Inspect the parser.");
    }
    f.scan();
    for (id, _, expected) in cases {
        assert_eq!(
            f.store.get(id).unwrap().unwrap().is_subagent,
            expected,
            "{id}"
        );
    }
}

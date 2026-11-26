use std::{fs, io::Write, path::PathBuf};

use agentsight_trajectory_collector::{
    scan_once, CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore,
};
use serde_json::{json, Value};

const ID: &str = "session-2026-10-27T09-00-ab12cd34";

struct Chat {
    root: PathBuf,
    file: PathBuf,
    config: CollectorConfig,
    store: TrajectoryStore,
}

impl Chat {
    fn new(prefix_type: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "gemini-notice-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let scan = root.join(".gemini/tmp");
        let chats = scan.join("project/chats");
        fs::create_dir_all(&chats).unwrap();
        let file = chats.join(format!("{ID}.jsonl"));
        let store = TrajectoryStore::new_with_path(&root.join("trajectories.db")).unwrap();
        let chat = Self {
            root,
            file,
            store,
            config: CollectorConfig {
                scan_interval_secs: 1,
                scan_dirs: Some(vec![scan]),
                maintenance: TrajectoryMaintenancePolicy::default(),
            },
        };
        for event in [
            json!({"sessionId":"fixture","projectHash":"project"}),
            json!({"id":"n","type":prefix_type,"content":"CLI event"}),
            json!({"id":"u","type":"user","content":"old request"}),
            json!({"id":"a","type":"gemini","content":"old answer","tokens":{"input":10,"output":5}}),
        ] {
            chat.append(event);
        }
        chat.scan();
        let record = chat.store.get(ID).unwrap().unwrap();
        assert_eq!(record.num_steps, 2);
        assert_eq!(record.total_completion_tokens, Some(5));
        chat
    }

    fn append(&self, event: Value) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
            .unwrap();
        writeln!(file, "{event}").unwrap();
    }

    fn scan(&self) {
        scan_once(&self.store, &self.config);
    }

    fn assert_clear(&self) {
        assert_eq!(self.store.count().unwrap(), 0);
        assert_eq!(self.store.get_atif_json(ID).unwrap(), None);
    }

    fn state(&self) -> (i64, i64) {
        self.store
            .get_file_state(self.file.to_str().unwrap())
            .unwrap()
            .unwrap()
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn rewind_past_each_notice_clears_once_and_resumes_with_new_turn() {
    for kind in ["info", "warning", "error"] {
        let chat = Chat::new(kind);
        let initial = chat.state();
        chat.append(json!({"$rewindTo":"u"}));
        chat.scan();
        chat.assert_clear();
        let cleared = chat.state();
        assert_ne!(initial, cleared);
        chat.scan();
        assert_eq!(chat.state(), cleared);
        chat.assert_clear();

        chat.append(json!({"id":"new","type":"user","content":"new request"}));
        chat.scan();
        let record = chat.store.get(ID).unwrap().unwrap();
        assert_eq!(record.num_steps, 1);
        assert_eq!(record.first_user_message.as_deref(), Some("new request"));
        assert_eq!(record.total_completion_tokens, None);
        assert!(!record.atif_json.contains("old answer"));
    }
}

#[test]
fn patch_set_and_empty_metadata_clear_old_turns() {
    for reset in [
        json!({"$patch":{"removeIds":["u","a"]}}),
        json!({"$set":{"messages":[{"id":"n","type":"warning"}]}}),
    ] {
        let chat = Chat::new("info");
        chat.append(reset);
        chat.scan();
        chat.assert_clear();
    }

    let chat = Chat::new("error");
    fs::write(
        &chat.file,
        r#"{"sessionId":"fixture","projectHash":"project","messages":[]}"#,
    )
    .unwrap();
    chat.scan();
    chat.assert_clear();
}

#[test]
fn unknown_record_or_missing_rewind_target_preserves_snapshot() {
    for (kind, target) in [("future-kind", "u"), ("info", "missing")] {
        let chat = Chat::new(kind);
        let before = chat.store.get_atif_json(ID).unwrap();
        chat.append(json!({"$rewindTo":target}));
        chat.scan();
        assert_eq!(chat.store.get_atif_json(ID).unwrap(), before);
    }
}

#[test]
fn malformed_or_incomplete_input_preserves_snapshot() {
    let chat = Chat::new("warning");
    let before = chat.store.get_atif_json(ID).unwrap();
    chat.append(json!({"$rewindTo":"u"}));
    fs::OpenOptions::new()
        .append(true)
        .open(&chat.file)
        .unwrap()
        .write_all(br#"{"id":"#)
        .unwrap();
    chat.scan();
    assert_eq!(chat.store.get_atif_json(ID).unwrap(), before);
    fs::write(&chat.file, "broken JSON").unwrap();
    chat.scan();
    assert_eq!(chat.store.get_atif_json(ID).unwrap(), before);
}

#[test]
fn notice_only_rewrites_require_an_applied_reset() {
    for tail in [
        json!({"$patch":{"removeIds":["missing"]}}),
        json!({"$set":{"messages":[{"type":"info"}]}}),
        json!({"$set":{"messages":[{"id":"n","type":"warning"},null]}}),
        json!({"id":"n2","type":"error"}),
    ] {
        let chat = Chat::new("info");
        let before = chat.store.get_atif_json(ID).unwrap();
        fs::write(&chat.file, "{\"id\":\"n\",\"type\":\"info\"}\n").unwrap();
        chat.append(tail);
        chat.scan();
        assert_eq!(chat.store.get_atif_json(ID).unwrap(), before);
    }
}

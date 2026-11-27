//! A transient UI read failure must leave a Cline task eligible for the next scan.

use std::fs;
use std::path::PathBuf;

use agentsight_trajectory_collector::{
    cline, scan_once, CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore,
};

const UI_MESSAGES: &str = r#"[
        {"type":"say","say":"task","ts":1000,"conversationHistoryIndex":0},
        {"type":"say","say":"api_req_started","ts":2000,"conversationHistoryIndex":0,"text":"{\"tokensIn\":10,\"tokensOut\":5,\"cost\":0.01}"},
        {"type":"say","say":"text","ts":3000,"conversationHistoryIndex":1}
    ]"#;

struct Task(PathBuf);

impl Task {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("cline-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let task = Self(root);
        fs::create_dir_all(task.api().parent().unwrap()).unwrap();
        fs::write(
            task.api(),
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}]"#,
        )
        .unwrap();
        task
    }

    fn api(&self) -> PathBuf {
        self.0
            .join(cline::LINUX_TASKS_ROOT)
            .join("task")
            .join(cline::API_HISTORY_FILENAME)
    }

    fn store(&self) -> TrajectoryStore {
        TrajectoryStore::new_with_path(&self.0.join("t.db")).unwrap()
    }

    fn scan(&self, store: &TrajectoryStore) {
        scan_once(
            store,
            &CollectorConfig {
                scan_interval_secs: 1,
                scan_dirs: Some(vec![self.0.join(cline::LINUX_TASKS_ROOT)]),
                maintenance: TrajectoryMaintenancePolicy {
                    retention_days: 0,
                    ..Default::default()
                },
            },
        );
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_ui_ingests_api_history() {
    let task = Task::new("missing-ui");
    let store = task.store();
    task.scan(&store);
    let row = store.get("task").unwrap().unwrap();
    assert_eq!(row.num_steps, 2);
    assert!(row.total_prompt_tokens.is_none());
}

#[test]
fn previously_cached_incomplete_trajectory_is_refreshed() {
    let task = Task::new("cached-ui");
    let store = task.store();
    let api = task.api();
    let ui = api.with_file_name("ui_messages.json");
    task.scan(&store);
    let mut incomplete = store.get("task").unwrap().unwrap();
    assert!(incomplete.total_prompt_tokens.is_none());

    fs::write(&ui, UI_MESSAGES).unwrap();
    let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    for path in [&api, &ui] {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }
    // The v1 fingerprint of these fixture bytes, mtimes and default project.
    incomplete.file_size = -6_527_499_130_456_511_838;
    incomplete.file_mtime_ns = 1_700_000_000_000_000_000;
    store.upsert_trajectory(&incomplete).unwrap();
    task.scan(&store);
    assert_eq!(
        store.get("task").unwrap().unwrap().total_prompt_tokens,
        Some(10)
    );
}

#[cfg(unix)]
#[test]
fn unreadable_ui_retries_after_chmod() {
    use std::os::unix::fs::PermissionsExt;

    unsafe extern "C" {
        fn geteuid() -> std::ffi::c_uint;
    }
    // SAFETY: geteuid has no arguments or preconditions.
    if unsafe { geteuid() } == 0 {
        return;
    }

    struct RestoreMode(PathBuf);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o644));
        }
    }

    let task = Task::new("unreadable-ui");
    let store = task.store();
    let api = task.api();
    let ui = api.with_file_name("ui_messages.json");
    fs::write(&ui, UI_MESSAGES).unwrap();
    let before = fs::metadata(&ui).unwrap();
    let restore = RestoreMode(ui.clone());
    fs::set_permissions(&ui, fs::Permissions::from_mode(0o000)).unwrap();
    task.scan(&store);
    assert_eq!(store.count().unwrap(), 0);
    assert_eq!(store.get_file_state(api.to_str().unwrap()).unwrap(), None);

    drop(restore);
    let after = fs::metadata(&ui).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    task.scan(&store);
    let row = store.get("task").unwrap().unwrap();
    assert_eq!(row.start_time.as_deref(), Some("1970-01-01T00:00:01.000Z"));
    assert_eq!(row.end_time.as_deref(), Some("1970-01-01T00:00:03.000Z"));
    assert_eq!(row.total_prompt_tokens, Some(10));
    assert_eq!(row.total_completion_tokens, Some(5));
    let doc: serde_json::Value = serde_json::from_str(&row.atif_json).unwrap();
    assert_eq!(doc["final_metrics"]["total_cost_usd"], 0.01);
}

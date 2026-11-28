//! Grouped Claude-style replies must retain their final activity when collected.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use agentsight_trajectory_collector::{scan_once, CollectorConfig, TrajectoryStore};
use serde_json::{json, Value};

const SESSION: &str = "be0aa488-4e56-4604-bdf0-e12cc387392d";

struct Fixture {
    root: PathBuf,
    path: PathBuf,
    config: CollectorConfig,
    store: TrajectoryStore,
}

impl Fixture {
    fn new(source: &str, events: &[Value]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "claude-end-times-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let projects = root.join(format!(".{source}/projects"));
        let project = projects.join("-work");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{SESSION}.jsonl"));
        std::fs::write(
            &path,
            events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let store = TrajectoryStore::new_with_path(&root.join("collection.db")).unwrap();
        Self {
            root,
            path,
            store,
            config: CollectorConfig {
                scan_dirs: Some(vec![projects]),
                scan_interval_secs: 1,
                maintenance: Default::default(),
            },
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assistant(timestamp: &str, content: Value) -> Value {
    json!({"type":"assistant","timestamp":timestamp,"message":{"role":"assistant","content":content}})
}

#[test]
fn final_answer_after_tool_execution_ends_the_session() {
    let fixture = Fixture::new(
        "claude",
        &[
            assistant(
                "2026-11-08T00:00:01Z",
                json!([{"type":"tool_use","id":"call","name":"shell","input":{}}]),
            ),
            json!({"type":"user","timestamp":"2026-11-08T00:00:03Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"ok"}]}}),
            assistant(
                "2026-11-08T00:00:04Z",
                json!([{"type":"thinking","thinking":"summarize"}]),
            ),
            assistant(
                "2026-11-08T00:00:09Z",
                json!([{"type":"text","text":"done"}]),
            ),
        ],
    );
    scan_once(&fixture.store, &fixture.config);
    let row = fixture.store.get(SESSION).unwrap().unwrap();
    assert_eq!(row.start_time.as_deref(), Some("2026-11-08T00:00:01Z"));
    assert_eq!(row.end_time.as_deref(), Some("2026-11-08T00:00:09Z"));
    let document: Value = serde_json::from_str(&row.atif_json).unwrap();
    assert_eq!(document["steps"][1]["timestamp"], "2026-11-08T00:00:04Z");
    assert_eq!(document["steps"][1]["message"], "done");
}

#[test]
fn grouped_replies_keep_the_latest_valid_instant_for_all_claude_style_sources() {
    for source in ["claude", "qoder", "qoderwork", "cursor"] {
        let fixture = Fixture::new(
            source,
            &[
                assistant(
                    "2026-11-08T00:00:01Z",
                    json!([{"type":"thinking","thinking":"plan"}]),
                ),
                assistant(
                    "2026-11-08T00:00:04Z",
                    json!([{"type":"text","text":"answer"}]),
                ),
                json!({"type":"progress","timestamp":"2026-11-08T00:00:30Z"}),
                assistant(
                    "2026-11-08T08:00:02+08:00",
                    json!([{"type":"text","text":"earlier"}]),
                ),
                assistant("invalid", json!([{"type":"text","text":"untimed"}])),
            ],
        );
        scan_once(&fixture.store, &fixture.config);
        let row = fixture.store.get(SESSION).unwrap().unwrap();
        assert_eq!(row.num_steps, 1);
        assert_eq!(
            row.end_time.as_deref(),
            Some("2026-11-08T00:00:04Z"),
            "{source}"
        );
    }
}

#[test]
fn unchanged_pre_fix_snapshot_is_recollected_once() {
    let fixture = Fixture::new(
        "claude",
        &[
            assistant(
                "2026-11-08T00:00:01Z",
                json!([{"type":"thinking","thinking":"plan"}]),
            ),
            assistant(
                "2026-11-08T00:00:04Z",
                json!([{"type":"text","text":"done"}]),
            ),
        ],
    );
    scan_once(&fixture.store, &fixture.config);
    let mut old = fixture.store.get(SESSION).unwrap().unwrap();
    old.end_time = old.start_time.clone();
    old.file_size = i64::try_from(std::fs::metadata(&fixture.path).unwrap().len()).unwrap();
    fixture.store.upsert_trajectory(&old).unwrap();
    fixture
        .store
        .set_file_state(&old.file_path, old.file_size, old.file_mtime_ns)
        .unwrap();
    scan_once(&fixture.store, &fixture.config);
    let refreshed = fixture.store.get(SESSION).unwrap().unwrap();
    assert_eq!(refreshed.end_time.as_deref(), Some("2026-11-08T00:00:04Z"));
    assert!(refreshed.file_size < 0);
    scan_once(&fixture.store, &fixture.config);
    let settled = fixture.store.get(SESSION).unwrap().unwrap();
    assert_eq!(settled.file_size, refreshed.file_size);
    assert_eq!(settled.file_mtime_ns, refreshed.file_mtime_ns);
}

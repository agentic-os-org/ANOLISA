//! Failed updates must settle without replacing the last usable trajectory.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agentsight_trajectory_collector::{
    scan_once, CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore,
};
use serde_json::json;

thread_local! {
    static PARSES: Cell<usize> = const { Cell::new(0) };
}

struct ParseCounter;

impl log::Log for ParseCounter {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if record
            .args()
            .to_string()
            .starts_with("Skipping malformed JSON on line")
        {
            PARSES.with(|count| count.set(count.get() + 1));
        }
    }

    fn flush(&self) {}
}

fn reset_parses() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        log::set_logger(&ParseCounter).unwrap();
        log::set_max_level(log::LevelFilter::Warn);
    });
    PARSES.with(|count| count.set(0));
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "agentsight-failed-update-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".codex/sessions")).unwrap();
        Self(root)
    }

    fn file(&self) -> PathBuf {
        self.0.join(".codex/sessions/rollout-cache-fixture.jsonl")
    }

    fn db(&self) -> PathBuf {
        self.0.join("trajectories.db")
    }

    fn config(&self) -> CollectorConfig {
        CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![self.0.join(".codex/sessions")]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        }
    }

    fn state(&self) -> (i64, i64) {
        let metadata = std::fs::metadata(self.file()).unwrap();
        (
            metadata.len() as i64,
            metadata
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i64,
        )
    }

    fn assert_state(&self, store: &TrajectoryStore) {
        assert_eq!(
            store.get_file_state(self.file().to_str().unwrap()).unwrap(),
            Some(self.state())
        );
    }

    fn ingest(&self) -> TrajectoryStore {
        std::fs::write(self.file(), valid_session()).unwrap();
        let store = TrajectoryStore::new_with_path(&self.db()).unwrap();
        scan_once(&store, &self.config());
        assert!(store.get("rollout-cache-fixture").unwrap().is_some());
        store
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn valid_session() -> String {
    [
        json!({"type":"session_meta","payload":{"id":"cache-fixture","cwd":"/workspace/demo","cli_version":"fixture"}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Inspect the project."}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Project inspected."}]}}),
    ].iter().map(|event| format!("{event}\n")).collect()
}

#[test]
fn failed_update_parses_once_across_scan_and_maintenance_rounds() {
    reset_parses();
    let fixture = Fixture::new();
    let store = fixture.ingest();
    let original = store.get("rollout-cache-fixture").unwrap().unwrap();
    std::fs::write(fixture.file(), "not-json\n").unwrap();
    for _ in 0..5 {
        scan_once(&store, &fixture.config());
        store
            .maintain(TrajectoryMaintenancePolicy::default())
            .unwrap();
    }
    assert_eq!(
        PARSES.with(Cell::get),
        1,
        "unchanged failed updates should be parsed once"
    );
    fixture.assert_state(&store);
    assert_eq!(
        store
            .get("rollout-cache-fixture")
            .unwrap()
            .unwrap()
            .atif_json,
        original.atif_json
    );
}

#[test]
fn fresh_invalid_file_still_settles() {
    reset_parses();
    let fixture = Fixture::new();
    std::fs::write(fixture.file(), "not-json\n").unwrap();
    let store = TrajectoryStore::new_with_path(&fixture.db()).unwrap();
    for _ in 0..5 {
        scan_once(&store, &fixture.config());
        store
            .maintain(TrajectoryMaintenancePolicy::default())
            .unwrap();
    }
    assert_eq!(PARSES.with(Cell::get), 1);
    fixture.assert_state(&store);
    assert!(store.get("rollout-cache-fixture").unwrap().is_none());
}

#[test]
fn metadata_only_update_settles_and_original_version_can_be_restored() {
    let fixture = Fixture::new();
    let store = fixture.ingest();
    let modified = std::fs::metadata(fixture.file())
        .unwrap()
        .modified()
        .unwrap();
    let original = store.get("rollout-cache-fixture").unwrap().unwrap();
    std::fs::write(
        fixture.file(),
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"metadata-only\"}}\n",
    )
    .unwrap();
    // A restored/copied file can have an older timestamp than the saved version.
    std::fs::File::options()
        .write(true)
        .open(fixture.file())
        .unwrap()
        .set_modified(modified - Duration::from_secs(1))
        .unwrap();
    scan_once(&store, &fixture.config());
    fixture.assert_state(&store);
    store
        .maintain(TrajectoryMaintenancePolicy::default())
        .unwrap();
    fixture.assert_state(&store);
    assert_eq!(
        store
            .get("rollout-cache-fixture")
            .unwrap()
            .unwrap()
            .atif_json,
        original.atif_json
    );

    // Restore exactly the original size/mtime: the last failed scan must still
    // cause this valid version to be ingested rather than matching stale state.
    rusqlite::Connection::open(fixture.db())
        .unwrap()
        .execute("UPDATE collected_trajectories SET collected_at_ns = 1", [])
        .unwrap();
    std::fs::write(fixture.file(), valid_session()).unwrap();
    std::fs::File::options()
        .write(true)
        .open(fixture.file())
        .unwrap()
        .set_modified(modified)
        .unwrap();
    scan_once(&store, &fixture.config());
    fixture.assert_state(&store);
    assert!(store.list_summaries(None, None, None, 1).unwrap()[0].collected_at_ns > 1);
}

#[test]
fn same_size_failed_update_uses_the_observed_timestamp() {
    let fixture = Fixture::new();
    let store = fixture.ingest();
    let modified = std::fs::metadata(fixture.file())
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(fixture.file(), "x".repeat(valid_session().len())).unwrap();
    std::fs::File::options()
        .write(true)
        .open(fixture.file())
        .unwrap()
        .set_modified(modified + Duration::from_secs(1))
        .unwrap();
    scan_once(&store, &fixture.config());
    fixture.assert_state(&store);
    store
        .maintain(TrajectoryMaintenancePolicy::default())
        .unwrap();
    fixture.assert_state(&store);
}

#[test]
fn retention_eviction_preserves_failed_update_state() {
    let fixture = Fixture::new();
    let store = fixture.ingest();
    std::fs::write(fixture.file(), "not-json\n").unwrap();
    scan_once(&store, &fixture.config());
    rusqlite::Connection::open(fixture.db())
        .unwrap()
        .execute("UPDATE collected_trajectories SET collected_at_ns = 1", [])
        .unwrap();
    let report = store
        .maintain(TrajectoryMaintenancePolicy {
            retention_days: 1,
            max_db_size_mb: 0,
        })
        .unwrap();
    assert_eq!(report.expired_trajectories, 1);
    assert!(store.get("rollout-cache-fixture").unwrap().is_none());
    fixture.assert_state(&store);
}

#[test]
fn capacity_eviction_preserves_failed_update_state() {
    let fixture = Fixture::new();
    let store = fixture.ingest();
    std::fs::write(fixture.file(), "not-json\n").unwrap();
    scan_once(&store, &fixture.config());
    rusqlite::Connection::open(fixture.db())
        .unwrap()
        .execute(
            "UPDATE collected_trajectories SET atif_json = CAST(zeroblob(2000000) AS TEXT)",
            [],
        )
        .unwrap();
    let report = store
        .maintain(TrajectoryMaintenancePolicy {
            retention_days: 0,
            max_db_size_mb: 1,
        })
        .unwrap();
    assert!(report.size.deleted_rows > 0);
    fixture.assert_state(&store);
}

#[test]
fn redundant_state_falls_back_to_trajectory_and_missing_failed_file_is_removed() {
    let fixture = Fixture::new();
    let store = fixture.ingest();
    let report = store
        .maintain(TrajectoryMaintenancePolicy::default())
        .unwrap();
    assert_eq!(report.removed_skipped_files, 1);
    fixture.assert_state(&store);
    std::fs::write(fixture.file(), "not-json\n").unwrap();
    scan_once(&store, &fixture.config());
    std::fs::remove_file(fixture.file()).unwrap();
    let report = store
        .maintain(TrajectoryMaintenancePolicy::default())
        .unwrap();
    assert_eq!(report.removed_skipped_files, 1);
    assert!(store.get("rollout-cache-fixture").unwrap().is_some());
}

//! Trajectory collector — periodically scans Qoder/QoderWork session
//! directories, converts JSONL trajectories to ATIF v1.7 and persists them
//! into a dedicated SQLite database.
//!
//! Pure user-space (no eBPF, no inotify): the main crate spawns
//! [`run_collector_loop`] on a named thread when
//! `features.trajectory_collection.enabled` is set; the shared stop flag
//! (`running`) terminates the loop on shutdown.

pub mod atif;
pub mod codex;
pub mod discovery;
pub mod qoder;
pub mod store;

pub use store::{
    strip_system_context, StepContext, StepHit, StepScanFilter, StepScanOutcome, StepView,
    TrajectoryAgentActivitySummary, TrajectoryFilters, TrajectoryMaintenancePolicy,
    TrajectoryMaintenanceReport, TrajectoryRecord, TrajectoryStore, TrajectorySummary,
};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};

use discovery::DiscoveredSession;

/// Collector runtime configuration (resolved by the main crate from
/// `agentsight.json`).
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Seconds between two scan rounds.
    pub scan_interval_secs: u64,
    /// Optional override of the projects directories to scan; `None` probes
    /// `/root` + `/home/*` for the default Qoder/QoderWork layout.
    pub scan_dirs: Option<Vec<PathBuf>>,
    /// Retention used to skip already-expired source files during scans.
    pub maintenance: TrajectoryMaintenancePolicy,
}

/// Scan-convert-persist loop; returns when `stop` is cleared.
///
/// Runs one scan immediately, then sleeps `scan_interval_secs` between rounds
/// (checking `stop` every second). Database lifecycle is owned by the caller's
/// maintenance worker.
pub fn run_collector_loop(
    store: std::sync::Arc<TrajectoryStore>,
    config: &CollectorConfig,
    stop: &AtomicBool,
) {
    log::info!(
        "Trajectory collector started (interval={}s)",
        config.scan_interval_secs
    );

    // Single exit point: the loop always exits via the `sleep_or_stop` check,
    // avoiding split semantics between the while-condition and the helper.
    loop {
        scan_once(&store, config);
        if !sleep_or_stop(stop, config.scan_interval_secs) {
            break;
        }
    }
    log::info!("Trajectory collector stopped");
}

/// One scan round: discover sessions and ingest new/changed files.
/// Per-file failures are logged and skipped so one bad session never blocks
/// the rest of the round.
pub fn scan_once(store: &TrajectoryStore, config: &CollectorConfig) {
    let sessions = discovery::discover_sessions(config.scan_dirs.as_deref());
    let mut collected = 0usize;
    for session in &sessions {
        match process_session(store, session, config.maintenance.retention_days) {
            Ok(true) => collected += 1,
            Ok(false) => {}
            Err(e) => {
                log::warn!(
                    "Trajectory collect failed for {}: {e}",
                    session.path.display()
                );
            }
        }
    }
    if collected > 0 {
        log::info!(
            "Trajectory scan: {collected}/{} session(s) ingested",
            sessions.len()
        );
    } else {
        log::debug!(
            "Trajectory scan: {} session(s), none changed",
            sessions.len()
        );
    }
}

/// Ingest a single session file. Returns `Ok(true)` when the row was
/// (re)written, `Ok(false)` when the file is unchanged.
fn process_session(
    store: &TrajectoryStore,
    session: &DiscoveredSession,
    retention_days: u64,
) -> Result<bool> {
    let meta = std::fs::metadata(&session.path)
        .with_context(|| format!("stat {}", session.path.display()))?;
    let file_size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let file_mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0);

    let file_path = session.path.to_string_lossy().to_string();
    if retention_days > 0 {
        let now_ns = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let cutoff_ns = agentsight_sqlite_lifecycle::retention_cutoff_ns(now_ns, retention_days)?;
        if file_mtime_ns < i64::try_from(cutoff_ns).unwrap_or(i64::MAX) {
            store.set_file_state(&file_path, file_size, file_mtime_ns)?;
            return Ok(false);
        }
    }
    if let Some((size, mtime)) = store.get_file_state(&file_path)? {
        if size == file_size && mtime == file_mtime_ns {
            return Ok(false);
        }
    }

    let content = std::fs::read_to_string(&session.path)
        .with_context(|| format!("read {}", session.path.display()))?;
    let events = qoder::load_jsonl_events(&content);
    if events.is_empty() {
        // Record file state so persistently corrupted files are not re-read
        // every scan round (they will be retried only when size/mtime change).
        let _ = store.set_file_state(&file_path, file_size, file_mtime_ns);
        anyhow::bail!("no valid JSONL events");
    }

    // Codex rollout files use an envelope schema (`{timestamp,type,payload}`)
    // that needs a dedicated converter; everything else shares the
    // Claude-style converter.
    let is_codex = session.source == "codex" || codex::is_codex_rollout(&events);
    let mut trajectory = if is_codex {
        codex::convert_codex_events(&events, &session.source)?
    } else {
        atif::convert_qoder_events(&events, &session.source)?
    };
    if trajectory.steps.is_empty() {
        let _ = store.set_file_state(&file_path, file_size, file_mtime_ns);
        anyhow::bail!(
            "no ATIF steps extracted for source {}; converter support may be missing",
            session.source
        );
    }
    // The discovered id (filename UUID, subagent-composite when nested) is the
    // canonical per-file identity; keep the ATIF document aligned with the
    // primary-key column so both always agree.
    trajectory.session_id = Some(session.session_id.clone());
    // Agent-private info (cwd, message counts, project) rides in `extra`.
    let private = if is_codex {
        codex::extract_private_metadata(&events, &session.project)
    } else {
        qoder::extract_private_metadata(&events, &session.project)
    };
    let extra = trajectory.extra.get_or_insert_with(Default::default);
    extra.extend(private);
    // Codex's flat layout has no per-project directories; use the project
    // derived from the session cwd instead of the "(default)" placeholder.
    let project = extra
        .get("project")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| session.project.clone());

    let atif_json = serde_json::to_string(&trajectory.to_json_value()?)?;
    // Derived preview columns reuse the same extractor as the legacy-row
    // backfill so both code paths always agree.
    let (first_user_message, last_user_message) = store::extract_user_message_previews(&atif_json);
    let record = TrajectoryRecord {
        session_id: session.session_id.clone(),
        schema_version: trajectory.schema_version.clone(),
        agent_name: trajectory.agent.name.clone(),
        model_name: trajectory.agent.model_name.clone(),
        num_steps: i64::try_from(trajectory.steps.len()).unwrap_or(i64::MAX),
        total_prompt_tokens: trajectory
            .final_metrics
            .as_ref()
            .and_then(|m| m.total_prompt_tokens)
            .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        total_completion_tokens: trajectory
            .final_metrics
            .as_ref()
            .and_then(|m| m.total_completion_tokens)
            .map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        start_time: trajectory.steps.iter().find_map(|s| s.timestamp.clone()),
        end_time: trajectory
            .steps
            .iter()
            .rev()
            .find_map(|s| s.timestamp.clone()),
        first_user_message,
        last_user_message,
        atif_json,
        project,
        source: session.source.clone(),
        is_subagent: session.is_subagent,
        file_path,
        file_size,
        file_mtime_ns,
    };
    store.upsert_trajectory(&record)?;
    Ok(true)
}

/// Sleep `interval_secs` in 1s slices; returns `false` as soon as `stop` is
/// cleared (mirrors the main crate's `utils::thread::sleep_or_stop`).
fn sleep_or_stop(stop: &AtomicBool, interval_secs: u64) -> bool {
    for _ in 0..interval_secs {
        std::thread::sleep(Duration::from_secs(1));
        if !stop.load(Ordering::SeqCst) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const UUID_A: &str = "be0aa488-4e56-4604-bdf0-e12cc387392d";

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("traj-loop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_session(projects: &std::path::Path) -> PathBuf {
        let proj = projects.join("-data-myapp");
        std::fs::create_dir_all(&proj).unwrap();
        let path = proj.join(format!("{UUID_A}.jsonl"));
        let content = concat!(
            "{\"type\":\"runtime-config\",\"sessionId\":\"abc-123\",\"model\":\"qwen-max\"}\n",
            "{\"type\":\"user\",\"cwd\":\"/data/myapp\",\"timestamp\":\"2026-07-25T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-25T10:00:02Z\",\"message\":{\"role\":\"assistant\",\"model\":\"qwen-max\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}\n",
        );
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_scan_once_ingests_and_skips_unchanged() {
        let base = tmp_dir("scan");
        let projects = base.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        write_session(&projects);

        let store = TrajectoryStore::new_with_path(&base.join("t.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![projects.clone()]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };

        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);

        let rec = store.get(UUID_A).unwrap().unwrap();
        // Derived columns must agree with the persisted ATIF document.
        let doc: serde_json::Value = serde_json::from_str(&rec.atif_json).unwrap();
        assert_eq!(doc["schema_version"], rec.schema_version);
        assert_eq!(doc["session_id"], rec.session_id);
        assert_eq!(doc["agent"]["model_name"], "qwen-max");
        assert_eq!(doc["steps"].as_array().unwrap().len() as i64, rec.num_steps);
        assert_eq!(rec.total_prompt_tokens, Some(10));
        assert_eq!(rec.start_time.as_deref(), Some("2026-07-25T10:00:00Z"));
        assert_eq!(rec.end_time.as_deref(), Some("2026-07-25T10:00:02Z"));
        // Qoder-private info lives in extra
        assert_eq!(doc["extra"]["cwd"], "/data/myapp");
        assert_eq!(doc["extra"]["project"], "data-myapp");

        // Second scan with unchanged file must not fail and keeps one row.
        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_run_collector_loop_stops_on_flag() {
        let base = tmp_dir("loop");
        let projects = base.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        write_session(&projects);

        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![projects]),
            maintenance: TrajectoryMaintenancePolicy {
                retention_days: 1,
                max_db_size_mb: 1,
            },
        };
        let db_path = base.join("t.db");
        let store = Arc::new(TrajectoryStore::new_with_path(&db_path).unwrap());
        scan_once(&store, &config);
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute("UPDATE collected_trajectories SET collected_at_ns = 1", [])
            .unwrap();
        let loop_store = Arc::clone(&store);
        let stop = Arc::new(AtomicBool::new(true));
        let stop_clone = Arc::clone(&stop);
        let config_clone = config.clone();
        let handle = std::thread::spawn(move || {
            run_collector_loop(loop_store, &config_clone, &stop_clone);
        });

        std::thread::sleep(Duration::from_millis(1500));
        stop.store(false, Ordering::SeqCst);
        handle.join().unwrap();

        // The caller-owned row remains even though its collection timestamp is
        // expired, proving the collector loop never runs lifecycle maintenance.
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_scan_once_skips_empty_steps_session() {
        let base = tmp_dir("empty-steps");
        let projects = base.join("projects");
        let proj = projects.join("-data-empty");
        std::fs::create_dir_all(&proj).unwrap();
        // JSONL with only metadata — no user/assistant events → 0 ATIF steps.
        std::fs::write(
            proj.join(format!("{UUID_A}.jsonl")),
            "{\"type\":\"runtime-config\",\"sessionId\":\"empty-1\",\"model\":\"test\"}\n{\"type\":\"session_meta\",\"foo\":\"bar\"}\n",
        )
        .unwrap();

        let store = TrajectoryStore::new_with_path(&base.join("t.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![projects]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };

        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_scan_once_routes_codex_rollout() {
        let base = tmp_dir("codex-route");
        // Flat Codex layout: <root>/.codex/sessions/YYYY/MM/rollout-*.jsonl
        let sessions_root = base.join(".codex").join("sessions");
        let nested = sessions_root.join("2026").join("08");
        std::fs::create_dir_all(&nested).unwrap();
        let content = concat!(
            "{\"timestamp\":\"2026-08-03T09:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"s-1\",\"cwd\":\"/w/demo-app\",\"cli_version\":\"0.146.0\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hi\"}}\n",
            "{\"timestamp\":\"2026-08-03T09:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}}\n",
        );
        std::fs::write(nested.join("rollout-c0ffee.jsonl"), content).unwrap();

        let store = TrajectoryStore::new_with_path(&base.join("t.db")).unwrap();
        let config = CollectorConfig {
            scan_interval_secs: 1,
            scan_dirs: Some(vec![sessions_root]),
            maintenance: TrajectoryMaintenancePolicy::default(),
        };

        scan_once(&store, &config);
        assert_eq!(store.count().unwrap(), 1);

        let rec = store.get("rollout-c0ffee").unwrap().unwrap();
        assert_eq!(rec.source, "codex");
        // Codex has no per-project directories: the project column must come
        // from the session cwd, not the "(default)" discovery placeholder.
        assert_eq!(rec.project, "demo-app");
        let doc: serde_json::Value = serde_json::from_str(&rec.atif_json).unwrap();
        assert_eq!(doc["agent"]["version"], "0.146.0");
        assert_eq!(doc["extra"]["cwd"], "/w/demo-app");
        assert_eq!(doc["steps"].as_array().unwrap().len(), 2);
        assert_eq!(rec.first_user_message.as_deref(), Some("hi"));
    }
}

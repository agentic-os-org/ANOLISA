use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use super::*;
use crate::recommendation::personal_model::{
    ActivityContext, AgentRequestBindingKind, AnalyzerLease, CandidateEvidenceSummary,
    CandidateSource, ContextAffinity, RedactionReport, ScopeKind, DISCLOSURE_VERSION,
};

fn test_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "cosh-personal-runtime-{name}-{}-{}",
        std::process::id(),
        random_hex(6).unwrap()
    ))
}

fn request_record(index: usize) -> ActivityRecord {
    ActivityRecord {
        activity_id: format!("activity-{index}"),
        session_scope_id: Some("session".to_string()),
        source_fingerprint: format!("fingerprint-{index}"),
        observed_hour_bucket: 1_000,
        source: ActivitySource::AgentRequest,
        context: ActivityContext::default(),
        payload: ActivityPayload::AgentRequest {
            text: format!("request {index}"),
            binding: AgentRequestBindingKind::FreeForm,
            context_command_activity_id: None,
            intent_lifecycle_id: format!("intent-{index}"),
            system_recommended_skill: None,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

fn open_noticed_runtime(root: &Path, now_hour_bucket: u64) -> PersonalRuntime {
    let mut runtime = PersonalRuntime::open(true, root, now_hour_bucket).unwrap();
    runtime
        .mark_notice_seen(DISCLOSURE_VERSION, now_hour_bucket)
        .unwrap();
    runtime
}

fn history_record(index: usize) -> ActivityRecord {
    let mut record = request_record(index);
    record.source = ActivitySource::BashHistory;
    record.payload = ActivityPayload::BashHistoryCommand {
        command: format!("history {index}"),
        origin_unverified: true,
        execution_hour_bucket: None,
        time_unverified: true,
    };
    record
}

#[test]
fn inert_runtime_never_accepts_or_persists_records() {
    let mut runtime = PersonalRuntime::inert();
    assert_eq!(runtime.enqueue(request_record(1)), EnqueueOutcome::Inactive);
    assert_eq!(runtime.flush_once(1_000).unwrap(), FlushOutcome::Idle);
    assert_eq!(runtime.status().queued_records, 0);
}

#[test]
fn bounded_queue_evicts_oldest_weak_record_before_strong_records() {
    let root = test_root("queue");
    let mut runtime = open_noticed_runtime(&root, 1_000);
    runtime.enqueue(history_record(0));
    for index in 1..MAX_QUEUE_ITEMS {
        runtime.enqueue(request_record(index));
    }

    assert_eq!(
        runtime.enqueue(request_record(21)),
        EnqueueOutcome::Accepted
    );
    assert_eq!(runtime.status().queued_records, MAX_QUEUE_ITEMS);
    assert_eq!(runtime.status().dropped_records.bash_history, 1);
    assert!(runtime
        .queue
        .iter()
        .all(|item| item.record.source != ActivitySource::BashHistory));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_commit_keeps_queue_and_later_flush_persists_it_once() {
    let root = test_root("retry");
    let mut runtime = open_noticed_runtime(&root, 1_000);
    runtime.enqueue(request_record(1));
    fs::set_permissions(&root, fs::Permissions::from_mode(0o750)).unwrap();

    assert!(runtime.flush_once(1_001).is_err());
    assert_eq!(runtime.status().queued_records, 1);

    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        runtime.flush_once(1_001).unwrap(),
        FlushOutcome::Persisted(1)
    );
    assert_eq!(runtime.status().queued_records, 0);
    assert_eq!(runtime.status().persisted_records, 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clear_rotates_epoch_drops_retry_queue_and_unbinds_feedback() {
    let root = test_root("clear");
    let mut runtime = open_noticed_runtime(&root, 1_000);
    let prior_epoch = runtime.snapshot().unwrap().store_epoch.clone();
    runtime.enqueue(request_record(1));
    runtime.freeze_prompt(binding());

    runtime.clear(1_001).unwrap();

    assert_ne!(runtime.snapshot().unwrap().store_epoch, prior_epoch);
    assert!(runtime.snapshot().unwrap().journal.records.is_empty());
    assert_eq!(runtime.status().queued_records, 0);
    assert!(runtime.accept_frozen_prompt().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clear_retries_after_a_concurrent_lease_and_stops_the_new_lease() {
    let root = test_root("clear-concurrent-lease");
    let store = PersonalStore::open(&root).unwrap();
    let initial = store.initialize(1_000).unwrap();
    let mut settled = Vec::new();
    let mut inserted = false;

    let cleared = clear_store_with_retry(&store, 1_001, |observed| {
        if let Some(lease) = observed {
            settled.push(lease.lease_nonce.clone());
        } else if !inserted {
            inserted = true;
            store.merge(&StateVersion::of(&initial), 1_001, |state| {
                state.scheduler.lease = Some(runtime_test_lease(state, "new-lease"));
            })?;
        }
        Ok(())
    })
    .unwrap();

    assert_eq!(settled, ["new-lease"]);
    assert_ne!(cleared.store_epoch, initial.store_epoch);
    assert!(cleared.scheduler.lease.is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clear_retry_is_bounded_and_preserves_the_latest_lease_on_failure() {
    let root = test_root("clear-bounded");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    let mut attempts = 0;

    let error = clear_store_with_retry(&store, 1_001, |_| {
        attempts += 1;
        let current = store.load(1_001)?.unwrap();
        store.merge(&StateVersion::of(&current), 1_001, |state| {
            state.scheduler.lease =
                Some(runtime_test_lease(state, &format!("new-lease-{attempts}")));
        })?;
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        error,
        PersonalRuntimeError::Store(StoreError::StaleState)
    ));
    assert_eq!(attempts, MAX_CLEAR_CAS_ATTEMPTS);
    assert_eq!(
        store
            .load(1_001)
            .unwrap()
            .unwrap()
            .scheduler
            .lease
            .unwrap()
            .lease_nonce,
        "new-lease-3"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn history_reset_removes_persisted_and_queued_history_and_profile() {
    let root = test_root("history-reset");
    let mut runtime = open_noticed_runtime(&root, 1_000);
    runtime.enqueue(history_record(1));
    runtime.flush_once(1_000).unwrap();
    runtime.enqueue(history_record(2));

    runtime.reset_history_derived(1_001).unwrap();

    assert!(runtime.snapshot().unwrap().journal.records.is_empty());
    assert_eq!(runtime.status().queued_records, 0);
    assert!(runtime.snapshot().unwrap().journal.history_baseline_pending);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn planner_mapping_uses_cache_snapshot_and_feedback_suppression() {
    let candidate = CachedPromptCandidate {
        candidate_id: "candidate".to_string(),
        source: CandidateSource::RecentTask,
        task_ref: "task".to_string(),
        prompt_text: "continue task".to_string(),
        context_affinity: ContextAffinity {
            scope_kind: ScopeKind::Repo,
            repo_id: Some("repo".to_string()),
            host_id: Some("host".to_string()),
        },
        last_seen_hour_bucket: 999,
        last_action_failed: false,
        evidence: CandidateEvidenceSummary::default(),
        entities: Vec::new(),
    };
    let mut runtime = PersonalRuntime::inert();
    let mut state = RecommendationState::empty("epoch".to_string(), 1_000);
    state.cache.candidates.push(candidate);
    state.feedback.push(RecommendationFeedbackState {
        task_ref: "task".to_string(),
        last_impression_hour_bucket: Some(999),
        last_submitted_hour_bucket: None,
        consecutive_explicit_dismissals: 0,
        last_explicit_dismissal_hour_bucket: None,
        consecutive_overrides: 0,
        last_override_hour_bucket: None,
    });
    runtime.state = Some(state);

    let candidates = runtime.planner_candidates(1_000);

    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].suppressed);
    assert_eq!(candidates[0].task_ref, "task");
}

#[test]
fn shutdown_flushes_then_stops_accepting_new_records() {
    let root = test_root("shutdown");
    let mut runtime = open_noticed_runtime(&root, 1_000);
    runtime.enqueue(request_record(1));

    assert_eq!(runtime.shutdown(1_000, Duration::from_secs(1)).unwrap(), 1);
    assert_eq!(runtime.enqueue(request_record(2)), EnqueueOutcome::Inactive);
    assert_eq!(runtime.status().persisted_records, 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn persisted_off_beats_environment_on_and_environment_off_is_absolute() {
    let root = test_root("enablement-priority");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    store.set_user_enabled(false, 1_001).unwrap();

    let runtime = PersonalRuntime::open_with_environment(true, Some(true), &root, 1_002).unwrap();
    assert!(!runtime.status().enabled);
    drop(runtime);

    store.set_user_enabled(true, 1_003).unwrap();
    let runtime = PersonalRuntime::open_with_environment(true, Some(false), &root, 1_004).unwrap();
    assert!(!runtime.status().enabled);
    assert_eq!(
        runtime.snapshot().unwrap().preferences.user_enabled,
        Some(true)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn background_writer_persists_records_without_foreground_flush() {
    let root = test_root("background");
    let now = current_hour_bucket();
    let runtime = PersonalRuntime::open(true, &root, now).unwrap();
    let mut writer = runtime.spawn_writer().unwrap();
    writer
        .mark_notice_seen(DISCLOSURE_VERSION, now, Duration::from_secs(1))
        .unwrap();
    let mut record = request_record(1);
    record.observed_hour_bucket = now;

    assert_eq!(writer.try_enqueue(record), EnqueueOutcome::Accepted);
    let deadline = Instant::now() + Duration::from_secs(2);
    while writer
        .poll_status()
        .is_none_or(|status| status.persisted_records == 0)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(writer.poll_status().unwrap().persisted_records, 1);
    writer.shutdown(now, Duration::from_secs(1)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn background_enqueue_drops_instead_of_waiting_for_writer_lock() {
    let root = test_root("nonblocking");
    let runtime = PersonalRuntime::open(true, &root, 1_000).unwrap();
    let mut writer = runtime.spawn_writer().unwrap();
    let shared = Arc::clone(&writer.runtime);
    let guard = shared.lock().unwrap();

    assert_eq!(
        writer.try_enqueue(request_record(1)),
        EnqueueOutcome::Dropped
    );

    drop(guard);
    writer.shutdown(1_001, Duration::from_secs(1)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn background_writer_rejects_record_built_before_clear_epoch() {
    let root = test_root("stale-record");
    let runtime = PersonalRuntime::open(true, &root, 1_000).unwrap();
    let mut writer = runtime.spawn_writer().unwrap();
    writer
        .mark_notice_seen(DISCLOSURE_VERSION, 1_000, Duration::from_secs(1))
        .unwrap();
    let identity = writer
        .activity_identity(ActivitySource::AgentRequest, b"event")
        .unwrap()
        .unwrap();
    let mut record = request_record(1);
    record.activity_id = identity.activity_id;
    record.source_fingerprint = identity.source_fingerprint;

    writer.clear(1_001, Duration::from_secs(1)).unwrap();

    assert_eq!(
        writer.try_enqueue_for_epoch(record, &identity.store_epoch),
        EnqueueOutcome::Dropped
    );
    writer.shutdown(1_001, Duration::from_secs(1)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn writer_on_off_and_notice_share_the_main_state() {
    let root = test_root("writer-preferences");
    let runtime = PersonalRuntime::open(true, &root, 1_000).unwrap();
    let mut writer = runtime.spawn_writer().unwrap();
    writer
        .mark_notice_seen(DISCLOSURE_VERSION, 1_000, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        writer.try_enqueue(request_record(1)),
        EnqueueOutcome::Accepted
    );
    writer.shutdown(1_000, Duration::from_secs(1)).unwrap();

    let runtime = PersonalRuntime::open(true, &root, 1_001).unwrap();
    let mut writer = runtime.spawn_writer().unwrap();
    writer
        .set_user_enabled(false, 1_002, Duration::from_secs(1))
        .unwrap();
    assert!(!writer.poll_status().unwrap().enabled);
    assert!(writer.poll_snapshot().unwrap().journal.records.is_empty());
    writer
        .set_user_enabled(true, 1_003, Duration::from_secs(1))
        .unwrap();
    writer
        .mark_notice_seen(DISCLOSURE_VERSION, 1_004, Duration::from_secs(1))
        .unwrap();
    let snapshot = writer.poll_snapshot().unwrap();
    assert_eq!(snapshot.preferences.user_enabled, Some(true));
    assert_eq!(snapshot.preferences.notice_version_seen, DISCLOSURE_VERSION);
    writer.shutdown(1_004, Duration::from_secs(1)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn writer_rejects_activity_until_disclosure_is_persisted() {
    let root = test_root("writer-disclosure-gate");
    let mut runtime = PersonalRuntime::open(true, &root, 1_000).unwrap();

    assert_eq!(runtime.enqueue(request_record(1)), EnqueueOutcome::Inactive);
    runtime.mark_notice_seen(DISCLOSURE_VERSION, 1_001).unwrap();
    assert_eq!(runtime.enqueue(request_record(2)), EnqueueOutcome::Accepted);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn environment_force_off_rejects_on_without_changing_preference() {
    let root = test_root("writer-env-off");
    let store = PersonalStore::open(&root).unwrap();
    store.initialize(1_000).unwrap();
    store.set_user_enabled(false, 1_001).unwrap();
    let runtime = PersonalRuntime::open_with_environment(true, Some(false), &root, 1_002).unwrap();
    let mut writer = runtime.spawn_writer().unwrap();

    let error = writer
        .set_user_enabled(true, 1_003, Duration::from_secs(1))
        .unwrap_err();

    assert!(error.to_string().contains("COSH_RECOMMENDATIONS_ENABLED=0"));
    assert_eq!(
        writer.poll_snapshot().unwrap().preferences.user_enabled,
        Some(false)
    );
    writer.shutdown(1_003, Duration::from_secs(1)).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn history_records_keep_only_the_keyed_host_context() {
    let root = test_root("history-host");
    let history = root.join("bash_history");
    let mut runtime = open_noticed_runtime(&root, 1_000);
    fs::write(&history, "kubectl logs payment-api -n production\n").unwrap();

    runtime
        .sync_native_bash_history(
            &NativeBashHistoryMarker::new(history.clone()),
            unsafe { nix::libc::geteuid() },
            3_600_000,
            "machine-identity",
            &[],
        )
        .unwrap();
    fs::write(
        &history,
        "kubectl logs payment-api -n production\nsystemctl status payment-api\n",
    )
    .unwrap();
    runtime
        .sync_native_bash_history(
            &NativeBashHistoryMarker::new(history),
            unsafe { nix::libc::geteuid() },
            3_600_001,
            "machine-identity",
            &[],
        )
        .unwrap();

    let record = runtime
        .snapshot()
        .unwrap()
        .journal
        .records
        .iter()
        .find(|record| record.source == ActivitySource::BashHistory)
        .expect("history record");
    assert!(record
        .context
        .host_id
        .as_deref()
        .unwrap()
        .starts_with("host:hmac:v1:"));
    assert!(record.context.repo_id.is_none());
    assert!(record.context.repo_name.is_none());
    assert!(record.context.cwd_relative.is_none());
    assert!(record.session_scope_id.is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn disabled_runtime_does_not_read_native_bash_history() {
    let root = test_root("disabled-history-read");
    let history_directory = root.join("history-directory");
    let mut runtime = PersonalRuntime::open(true, &root, 1_000).unwrap();
    runtime.set_user_enabled(false, 1_001).unwrap();
    fs::create_dir_all(&history_directory).unwrap();

    runtime
        .sync_native_bash_history(
            &NativeBashHistoryMarker::new(history_directory),
            unsafe { nix::libc::geteuid() },
            3_600_000,
            "machine-identity",
            &[],
        )
        .expect("disabled runtime must not inspect the history path");

    assert!(runtime.snapshot().unwrap().journal.records.is_empty());
    fs::remove_dir_all(root).unwrap();
}

fn runtime_test_lease(state: &RecommendationState, nonce: &str) -> AnalyzerLease {
    AnalyzerLease {
        owner_session_id: "new-session".to_string(),
        lease_nonce: nonce.to_string(),
        owner_pid: 10,
        owner_start_identity: "owner-start".to_string(),
        core_leader_pid: Some(20),
        core_leader_start_identity: Some("leader-start".to_string()),
        core_process_group_id: Some(20),
        base_epoch: state.store_epoch.clone(),
        base_generation: state.generation,
        expires_unix_secs: 2_000,
    }
}

fn binding() -> FrozenPromptBinding {
    FrozenPromptBinding {
        candidate_id: "candidate".to_string(),
        task_ref: "task".to_string(),
        original_prompt: "continue task".to_string(),
        source: CandidateSource::RecentTask,
        suppression_key: "task".to_string(),
        profile_generation: 1,
        intent_lifecycle_id: "intent".to_string(),
    }
}

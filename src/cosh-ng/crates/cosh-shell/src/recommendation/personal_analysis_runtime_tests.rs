use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::recommendation::personal_model::{
    ActivityContext, ActivityPayload, ActivityRecord, ActivitySource, AgentRequestBindingKind,
    AnalyzedFrequentPattern, AnalyzedRecentTask, AttemptPhase, DiscardReason, DiscardedActivity,
    EntityKind, EntityVolatility, ProfileAnalyzerResult, RedactionReport, TaskEntity,
    DISCLOSURE_VERSION,
};
use crate::recommendation::personal_runner::{InitializeResult, RunnerEvent};

#[test]
fn configured_run_consumes_session_and_finishes_attempt() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, AnalyzerRunOutcome::Completed);
    assert!(!gate.can_attempt());
    let state = store.state.borrow();
    assert_eq!(state.scheduler.attempts[0].phase, AttemptPhase::Finished);
    assert!(state.scheduler.lease.is_none());
    assert_eq!(dependencies.body_writes(), 1);
}

#[test]
#[ignore = "run after `cargo build -p cosh-core` for Gate 4 protocol evidence"]
fn gate4_real_core_uses_one_bare_toolless_request_without_touching_foreground_state() {
    const ARTIFACT_SENTINEL: &str = "gate4-activity-artifact-sentinel";
    let core = real_cosh_core_path();
    assert!(
        core.is_file(),
        "build cosh-core first: cargo build -p cosh-core ({})",
        core.display()
    );
    let root = Gate4TestDir::new();
    let home = root.path().join("home");
    let store_root = root.path().join("recommendations");
    fs::create_dir(&home).expect("create isolated HOME");
    let sls_path = root.path().join("cosh.sls.jsonl");
    fs::write(&sls_path, "").expect("precreate SLS log");

    let server = Gate4Provider::start("gate4-activity", ARTIFACT_SENTINEL);
    let wrapper = write_core_wrapper(root.path(), &core, &home, &sls_path, &server.base_url);
    let foreground_session = Arc::new(Mutex::new(
        crate::adapter::SessionRuntimeState::with_active("foreground-session", "/foreground/cwd"),
    ));
    let adapter = CoshCoreAdapter {
        program: wrapper.to_string_lossy().into_owned(),
        allow_model_call: false,
        session: foreground_session.clone(),
        ..CoshCoreAdapter::default()
    };

    let now_secs = 1_800_000_000;
    seed_gate4_state(
        &store_root,
        now_secs / 3600,
        "gate4-session",
        ARTIFACT_SENTINEL,
    );
    let mut gate = SessionGate::default();
    let outcome = run_analyzer_once_with_cancellation(
        true,
        &store_root,
        &adapter,
        &mut gate,
        "gate4-session",
        now_secs,
        AnalyzerTriggerContext {
            has_eligible_trigger: true,
            foreground_idle: true,
            foreground_activity_epoch: 0,
        },
        "foreground-model",
        AnalyzerCancellation::new(),
    );
    let requests = server.finish();

    assert_eq!(outcome, AnalyzerRunOutcome::Completed);
    assert!(!gate.can_attempt());
    assert_eq!(requests.len(), 1, "Analyzer must issue exactly one request");
    let provider_body: serde_json::Value =
        serde_json::from_slice(&requests[0]).expect("provider request JSON");
    assert_eq!(provider_body["model"], "foreground-model");
    assert!(
        provider_body.get("tools").is_none(),
        "--tools empty must omit provider tools"
    );
    assert!(
        provider_body["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().all(|message| message["role"] != "tool")),
        "Analyzer request must not contain a tool round"
    );
    assert!(
        provider_body.to_string().contains(ARTIFACT_SENTINEL),
        "bounded Analyzer body must reach the fake provider"
    );
    assert_eq!(
        foreground_session
            .lock()
            .expect("session lock")
            .active_session_id(),
        Some("foreground-session")
    );
    assert_eq!(
        foreground_session
            .lock()
            .expect("session lock")
            .active_workspace_scope(),
        Some("/foreground/cwd")
    );
    let log_dir = home.join(".copilot-shell/logs");
    assert!(
        log_dir.is_dir()
            && fs::read_dir(&log_dir)
                .expect("read debug log directory")
                .next()
                .is_some(),
        "real core must create a debug artifact for the sentinel check"
    );
    assert_artifacts_exclude(&home, ARTIFACT_SENTINEL);
    assert_artifacts_exclude_file(&sls_path, ARTIFACT_SENTINEL);
    let sls = fs::read_to_string(&sls_path).expect("read SLS artifact");
    let records = sls
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("SLS JSONL"))
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "Analyzer must emit one aggregate SLS turn"
    );
    assert_eq!(records[0]["session.api.total_requests"], 1);
    assert_eq!(records[0]["session.tool_call_counts.total"], 0);
}

#[test]
fn second_dispatch_in_same_session_is_rejected_before_spawn() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    let mut gate = SessionGate::default();

    assert_eq!(
        orchestrate_once(
            true,
            &store,
            &mut dependencies,
            &mut gate,
            "opaque-session",
            40_000,
            eligible(),
        ),
        AnalyzerRunOutcome::Completed
    );
    let second = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_001,
        eligible(),
    );

    assert_eq!(
        second,
        AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::Scheduler(SchedulerBlock::SessionConsumed))
    );
    assert_eq!(dependencies.spawns, 1);
}

#[test]
fn preflight_auth_failure_rolls_back_attempt_and_consumes_session() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.initialize = InitializeResult::AuthRequired;
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(
        outcome,
        AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::AuthNotConfigured)
    );
    let state = store.state.borrow();
    assert!(state.scheduler.attempts.is_empty());
    assert!(state.scheduler.lease.is_none());
    assert!(!gate.can_attempt());
}

#[test]
fn foreground_activity_after_spawn_rolls_back_without_writing_body() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.become_busy_on_spawn = true;
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, failed(AnalyzerFailureStage::Provider, false));
    assert_eq!(dependencies.body_writes(), 0);
    assert!(store.state.borrow().scheduler.attempts.is_empty());
    assert!(store.state.borrow().scheduler.lease.is_none());
    assert!(!gate.can_attempt());
}

#[test]
fn transient_foreground_activity_after_spawn_rolls_back_without_writing_body() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.become_busy_then_idle_on_spawn = true;
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, failed(AnalyzerFailureStage::Provider, false));
    assert_eq!(dependencies.body_writes(), 0);
    assert!(store.state.borrow().scheduler.attempts.is_empty());
    assert!(store.state.borrow().scheduler.lease.is_none());
    assert!(!gate.can_attempt());
}

#[test]
fn foreground_activity_at_body_claim_wins_before_body_write() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.become_busy_before_body_claim = true;
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, failed(AnalyzerFailureStage::Provider, false));
    assert_eq!(dependencies.body_writes(), 0);
}

#[test]
fn body_claim_release_preserves_foreground_activity_and_allows_next_claim() {
    let cancellation = AnalyzerCancellation::new();

    assert!(cancellation.claim_body_write(0));
    cancellation.set_foreground_idle(false);
    assert_eq!(cancellation.foreground_activity_epoch(), 1);
    cancellation.set_foreground_idle(true);
    assert!(!cancellation.claim_body_write(1));

    cancellation.release_body_write();
    assert!(cancellation.claim_body_write(1));
    cancellation.release_body_write();

    cancellation.set_foreground_idle(false);
    assert_eq!(cancellation.foreground_activity_epoch(), 2);
}

#[test]
fn rejected_body_claim_does_not_release_another_owner() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.body_claimed.set(true);

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut SessionGate::default(),
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, failed(AnalyzerFailureStage::Provider, false));
    assert!(dependencies.body_claimed.get());
    assert_eq!(dependencies.body_claim_releases.get(), 0);
}

#[test]
fn orchestration_releases_body_claim_after_success_write_failure_and_persist_failure() {
    let success_store = FakeStore::new(state_with_trigger());
    let mut success = FakeDependencies::success();
    assert_eq!(
        orchestrate_once(
            true,
            &success_store,
            &mut success,
            &mut SessionGate::default(),
            "opaque-session",
            40_000,
            eligible(),
        ),
        AnalyzerRunOutcome::Completed
    );
    assert_eq!(success.body_claim_releases.get(), 1);
    assert!(!success.body_claimed.get());

    let write_failure_store = FakeStore::new(state_with_trigger());
    let mut write_failure = FakeDependencies::success();
    write_failure.body_failure = Some(ProcessFailure::Transport);
    let _ = orchestrate_once(
        true,
        &write_failure_store,
        &mut write_failure,
        &mut SessionGate::default(),
        "opaque-session",
        40_000,
        eligible(),
    );
    assert_eq!(write_failure.body_claim_releases.get(), 1);
    assert!(!write_failure.body_claimed.get());

    let persist_failure_store = FakeStore::failing_commit(state_with_trigger(), 3);
    let mut persist_failure = FakeDependencies::success();
    let _ = orchestrate_once(
        true,
        &persist_failure_store,
        &mut persist_failure,
        &mut SessionGate::default(),
        "opaque-session",
        40_000,
        eligible(),
    );
    assert_eq!(persist_failure.body_claim_releases.get(), 1);
    assert!(!persist_failure.body_claimed.get());
}

#[test]
fn post_body_invalid_output_keeps_attempt_and_consumes_session() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.events = VecDeque::from([
        RunnerEvent::Assistant("not-json".to_string()),
        RunnerEvent::Result { success: true },
        RunnerEvent::End,
    ]);
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, failed(AnalyzerFailureStage::Output, true));
    assert!(!gate.can_attempt());
    assert_eq!(
        store.state.borrow().scheduler.attempts[0].phase,
        AttemptPhase::Finished
    );
}

#[test]
fn any_failure_after_body_write_starts_consumes_attempt_conservatively() {
    let zero_store = FakeStore::new(state_with_trigger());
    let mut zero_dependencies = FakeDependencies::success();
    zero_dependencies.body_failure = Some(ProcessFailure::Transport);
    let mut zero_gate = SessionGate::default();

    assert_eq!(
        orchestrate_once(
            true,
            &zero_store,
            &mut zero_dependencies,
            &mut zero_gate,
            "opaque-session",
            40_000,
            eligible(),
        ),
        failed(AnalyzerFailureStage::Provider, true)
    );
    assert_eq!(
        zero_store.state.borrow().scheduler.attempts[0].phase,
        AttemptPhase::Finished
    );
    assert!(!zero_gate.can_attempt());

    let partial_store = FakeStore::new(state_with_trigger());
    let mut partial_dependencies = FakeDependencies::success();
    partial_dependencies.body_failure = Some(ProcessFailure::TransportAfterWrite);
    let mut partial_gate = SessionGate::default();

    assert_eq!(
        orchestrate_once(
            true,
            &partial_store,
            &mut partial_dependencies,
            &mut partial_gate,
            "opaque-session",
            40_000,
            eligible(),
        ),
        failed(AnalyzerFailureStage::Provider, true)
    );
    assert_eq!(
        partial_store.state.borrow().scheduler.attempts[0].phase,
        AttemptPhase::Finished
    );
    assert!(!partial_gate.can_attempt());
}

#[test]
fn post_body_auth_failure_finishes_attempt() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.events = VecDeque::from([RunnerEvent::AuthRequired]);
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, failed(AnalyzerFailureStage::Provider, true));
    let state = store.state.borrow();
    assert_eq!(state.scheduler.attempts[0].phase, AttemptPhase::Finished);
    assert!(state.scheduler.lease.is_none());
    assert!(!gate.can_attempt());
}

#[test]
fn waits_for_async_writer_before_starting_provider() {
    let mut state = RecommendationState::empty("epoch".to_string(), 1);
    state.preferences.notice_version_seen = DISCLOSURE_VERSION;
    let store = FakeStore::with_delayed_trigger(state, 3, trigger_record());
    let mut dependencies = FakeDependencies::success();
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(outcome, AnalyzerRunOutcome::Completed);
    assert_eq!(dependencies.waits, 2);
    assert_eq!(dependencies.spawns, 1);
}

#[test]
fn writer_flush_timeout_does_not_spawn_or_consume_session() {
    let store = FakeStore::new(RecommendationState::empty("epoch".to_string(), 1));
    let mut dependencies = FakeDependencies::success();
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(
        outcome,
        AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::Scheduler(SchedulerBlock::NoTrigger))
    );
    assert_eq!(dependencies.spawns, 0);
    assert_eq!(dependencies.waits, WRITER_FLUSH_POLLS);
    assert!(gate.can_attempt());
}

#[test]
fn missing_auth_consumes_one_session_attempt_without_persistent_auth_state() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.initialize = InitializeResult::AuthRequired;
    let mut gate = SessionGate::default();

    let first = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );
    assert_eq!(
        first,
        AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::AuthNotConfigured)
    );
    assert_eq!(dependencies.spawns, 1);
    assert!(!gate.can_attempt());
}

#[test]
fn cancellation_failure_is_reported_and_keeps_the_lease() {
    let store = FakeStore::new(state_with_trigger());
    let mut dependencies = FakeDependencies::success();
    dependencies.initialize = InitializeResult::AuthRequired;
    dependencies.cancellation_failed = true;
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(
        outcome,
        AnalyzerRunOutcome::Failed {
            stage: AnalyzerFailureStage::Process,
            body_sent: false,
        }
    );
    assert!(store.state.borrow().scheduler.lease.is_some());
    assert!(!gate.can_attempt());
}

#[test]
fn cancellation_refuses_identity_mismatch_and_unchanged_epoch() {
    let cancellation = AnalyzerCancellation::new();
    cancellation.register(RunningAnalyzer {
        owner_pid: std::process::id(),
        owner_start_identity: "mismatched-owner".to_string(),
        owner_session_id: "session".to_string(),
        lease_nonce: "nonce".to_string(),
        leader_pid: std::process::id(),
        leader_start_identity: "mismatched-start".to_string(),
        process_group_id: std::process::id(),
        store_epoch: "epoch".to_string(),
    });

    assert!(!cancellation.cancel_current());
    assert!(!cancellation.cancel_if_guard_changed(&AnalyzerGuardHeader {
        version: 1,
        store_epoch: "epoch".to_string(),
        generation: 1,
        lease: Some(crate::recommendation::personal_store::AnalyzerGuardLease {
            owner_session_id: "session".to_string(),
            lease_nonce: "nonce".to_string(),
            owner_pid: std::process::id(),
            owner_start_identity: "mismatched-owner".to_string(),
            core_leader_pid: Some(std::process::id()),
            core_leader_start_identity: Some("mismatched-start".to_string()),
            core_process_group_id: Some(std::process::id()),
        }),
    }));
}

#[test]
fn embedded_schema_is_strict_and_fits_fixed_prompt_budget() {
    let schema: serde_json::Value = serde_json::from_str(ANALYZER_SCHEMA).expect("schema JSON");

    assert_eq!(
        schema.get("additionalProperties"),
        Some(&serde_json::json!(false))
    );
    assert_eq!(schema["required"].as_array().map(Vec::len), Some(3));
    assert!(build_fixed_prompt(ANALYZER_SCHEMA).is_ok());
}

#[test]
fn embedded_schema_matches_rust_result_golden() {
    let entity = TaskEntity {
        kind: EntityKind::Repo,
        value: "payment".to_string(),
        volatility: EntityVolatility::Stable,
    };
    let golden = ProfileAnalyzerResult {
        discarded_activities: vec![DiscardedActivity {
            activity_id: "activity-1".to_string(),
            reason: DiscardReason::NoRecommendationValue,
        }],
        recent_tasks: vec![AnalyzedRecentTask {
            prior_task_id: None,
            summary: "inspect payment".to_string(),
            entities: vec![entity.clone()],
            evidence_activity_ids: vec!["activity-1".to_string()],
            prior_snapshot_ids: Vec::new(),
            prompt_text: "continue inspection".to_string(),
        }],
        frequent_patterns: vec![AnalyzedFrequentPattern {
            prior_pattern_id: None,
            summary: "inspect payment regularly".to_string(),
            stable_entities: vec![entity],
            evidence_activity_ids: vec!["activity-1".to_string()],
            prior_snapshot_ids: Vec::new(),
            prompt_text: "inspect payment again".to_string(),
        }],
    };
    let value = serde_json::to_value(golden).expect("serialize Rust golden");
    let schema: serde_json::Value = serde_json::from_str(ANALYZER_SCHEMA).expect("schema JSON");

    assert_eq!(object_keys(&value), required_keys(&schema));
    assert_eq!(
        object_keys(&value["discarded_activities"][0]),
        required_keys(&schema["$defs"]["discarded"])
    );
    assert_eq!(
        object_keys(&value["recent_tasks"][0]),
        required_keys(&schema["$defs"]["recent"])
    );
    assert_eq!(
        object_keys(&value["frequent_patterns"][0]),
        required_keys(&schema["$defs"]["frequent"])
    );
    assert_eq!(
        schema["$defs"]["discarded"]["properties"]["reason"]["const"],
        serde_json::to_value(DiscardReason::NoRecommendationValue).unwrap()
    );
    assert_eq!(
        enum_values(&schema["$defs"]["entity"]["properties"]["kind"]),
        serialized_values([
            EntityKind::Namespace,
            EntityKind::Workload,
            EntityKind::Service,
            EntityKind::Repo,
            EntityKind::Branch,
            EntityKind::RelativePath,
            EntityKind::TestTarget,
            EntityKind::Process,
            EntityKind::Package,
            EntityKind::Host,
            EntityKind::Url,
        ])
    );
    assert_eq!(
        enum_values(&schema["$defs"]["entity"]["properties"]["volatility"]),
        serialized_values([EntityVolatility::Stable, EntityVolatility::Ephemeral])
    );
}

fn object_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .expect("JSON object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

fn required_keys(schema: &serde_json::Value) -> Vec<String> {
    let mut keys = schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .map(|value| value.as_str().expect("required string").to_string())
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

fn enum_values(schema: &serde_json::Value) -> Vec<String> {
    let mut values = schema["enum"]
        .as_array()
        .expect("enum array")
        .iter()
        .map(|value| value.as_str().expect("enum string").to_string())
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn serialized_values<T: serde::Serialize, const N: usize>(values: [T; N]) -> Vec<String> {
    let mut values = values
        .into_iter()
        .map(|value| {
            serde_json::to_value(value)
                .expect("serialize enum")
                .as_str()
                .expect("serialized enum string")
                .to_string()
        })
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn eligible() -> AnalyzerTriggerContext {
    AnalyzerTriggerContext {
        has_eligible_trigger: true,
        foreground_idle: true,
        foreground_activity_epoch: 0,
    }
}

fn state_with_trigger() -> RecommendationState {
    let mut state = RecommendationState::empty("epoch".to_string(), 1);
    state.preferences.notice_version_seen = DISCLOSURE_VERSION;
    state.journal.records.push(trigger_record());
    state
}

#[test]
fn notice_must_be_persisted_before_analyzer_can_spawn() {
    let mut state = state_with_trigger();
    state.preferences.notice_version_seen = 0;
    let store = FakeStore::new(state);
    let mut dependencies = FakeDependencies::success();
    let mut gate = SessionGate::default();

    let outcome = orchestrate_once(
        true,
        &store,
        &mut dependencies,
        &mut gate,
        "opaque-session",
        40_000,
        eligible(),
    );

    assert_eq!(
        outcome,
        AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::NoticeRequired)
    );
    assert_eq!(dependencies.spawns, 0);
}

fn trigger_record() -> ActivityRecord {
    ActivityRecord {
        activity_id: "activity-1".to_string(),
        session_scope_id: Some("opaque-session".to_string()),
        source_fingerprint: "source".to_string(),
        observed_hour_bucket: 1,
        source: ActivitySource::AgentRequest,
        context: ActivityContext::default(),
        payload: ActivityPayload::AgentRequest {
            text: "help inspect this service".to_string(),
            binding: AgentRequestBindingKind::FreeForm,
            context_command_activity_id: None,
            intent_lifecycle_id: "intent".to_string(),
            system_recommended_skill: None,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    }
}

fn real_cosh_core_path() -> PathBuf {
    std::env::current_exe()
        .expect("current test binary")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target profile directory")
        .join("cosh-core")
}

fn write_core_wrapper(
    root: &Path,
    core: &Path,
    home: &Path,
    sls_path: &Path,
    base_url: &str,
) -> PathBuf {
    let wrapper = root.join("cosh-core-gate4-wrapper.sh");
    let body = format!(
        "#!/bin/sh\nunset DASHSCOPE_API_KEY ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET ALIBABA_CLOUD_SECURITY_TOKEN\nexec env HOME={} COSH_AI_PROVIDER=gate4 COSH_MODEL=gate4-model OPENAI_BASE_URL={} OPENAI_API_KEY=gate4-test-key COSH_SLS_LOG_PATH={} COSH_LOG=debug {} \"$@\"\n",
        shell_quote(home),
        shell_quote(Path::new(base_url)),
        shell_quote(sls_path),
        shell_quote(core),
    );
    fs::write(&wrapper, body).expect("write core wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).expect("chmod core wrapper");
    wrapper
}

fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn seed_gate4_state(root: &Path, now_hour: u64, session_scope_id: &str, sentinel: &str) {
    let store = PersonalStore::open(root).expect("open Gate 4 store");
    let state = store.initialize(now_hour).expect("initialize Gate 4 store");
    let base = StateVersion::of(&state);
    let mut next = state;
    next.preferences.notice_version_seen = DISCLOSURE_VERSION;
    next.journal.records.push(ActivityRecord {
        activity_id: "gate4-activity".to_string(),
        session_scope_id: Some(session_scope_id.to_string()),
        source_fingerprint: "gate4-source".to_string(),
        observed_hour_bucket: now_hour,
        source: ActivitySource::AgentRequest,
        context: ActivityContext::default(),
        payload: ActivityPayload::AgentRequest {
            text: format!("inspect service {sentinel}"),
            binding: AgentRequestBindingKind::FreeForm,
            context_command_activity_id: None,
            intent_lifecycle_id: "gate4-intent".to_string(),
            system_recommended_skill: None,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    });
    store
        .commit(&base, next, now_hour)
        .expect("commit Gate 4 activity");
}

fn assert_artifacts_exclude(root: &Path, sentinel: &str) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path).expect("read artifact directory") {
            let entry = entry.expect("artifact entry");
            let kind = entry.file_type().expect("artifact type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                assert_artifacts_exclude_file(&entry.path(), sentinel);
            }
        }
    }
}

fn assert_artifacts_exclude_file(path: &Path, sentinel: &str) {
    let bytes = fs::read(path).expect("read artifact");
    assert!(
        !bytes
            .windows(sentinel.len())
            .any(|window| window == sentinel.as_bytes()),
        "artifact leaked Analyzer body: {}",
        path.display()
    );
}

struct Gate4TestDir {
    path: PathBuf,
}

impl Gate4TestDir {
    fn new() -> Self {
        static UNIQUE: AtomicUsize = AtomicUsize::new(1);
        let path = std::env::temp_dir().join(format!(
            "cosh-gate4-{}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&path).expect("create Gate 4 root");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Gate4TestDir {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("cosh-gate4-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct Gate4Provider {
    base_url: String,
    thread: Option<std::thread::JoinHandle<Vec<Vec<u8>>>>,
}

impl Gate4Provider {
    fn start(activity_id: &'static str, sentinel: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fake provider");
        let address = listener.local_addr().expect("fake provider address");
        let thread = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut requests = Vec::new();
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .expect("provider read timeout");
                        let body = read_http_body(&mut stream);
                        assert!(
                            body.windows(sentinel.len())
                                .any(|window| window == sentinel.as_bytes()),
                            "provider did not receive Analyzer sentinel"
                        );
                        requests.push(body);
                        write_provider_response(&mut stream, activity_id);
                        let second_request_deadline =
                            std::time::Instant::now() + Duration::from_millis(500);
                        while std::time::Instant::now() < second_request_deadline {
                            match listener.accept() {
                                Ok((mut second, _)) => {
                                    requests.push(read_http_body(&mut second));
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                Err(error) => panic!("fake provider accept: {error}"),
                            }
                        }
                        return requests;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("fake provider accept: {error}"),
                }
            }
            requests
        });
        Self {
            base_url: format!("http://{address}/v1"),
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Vec<Vec<u8>> {
        self.thread
            .take()
            .expect("fake provider thread")
            .join()
            .expect("fake provider completed")
    }
}

fn read_http_body(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut chunk).expect("read provider request");
        assert!(count > 0, "provider request ended before headers");
        received.extend_from_slice(&chunk[..count]);
        if let Some(index) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&received[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .expect("provider request content-length");
    while received.len() < header_end + content_length {
        let count = stream.read(&mut chunk).expect("read provider body");
        assert!(count > 0, "provider request body truncated");
        received.extend_from_slice(&chunk[..count]);
    }
    received[header_end..header_end + content_length].to_vec()
}

fn write_provider_response(stream: &mut std::net::TcpStream, activity_id: &str) {
    let analyzer_output = serde_json::json!({
        "discarded_activities": [{
            "activity_id": activity_id,
            "reason": "no_recommendation_value"
        }],
        "recent_tasks": [],
        "frequent_patterns": []
    })
    .to_string();
    let delta = serde_json::json!({
        "choices": [{
            "delta": {"content": analyzer_output},
            "finish_reason": null
        }]
    });
    let finish = serde_json::json!({
        "choices": [{"delta": {}, "finish_reason": "stop"}]
    });
    let body = format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .expect("write fake provider response");
    stream.flush().expect("flush fake provider response");
}

struct FakeStore {
    state: RefCell<RecommendationState>,
    delayed_trigger: RefCell<Option<(usize, ActivityRecord)>>,
    commits: std::cell::Cell<usize>,
    fail_on_commit: Option<usize>,
}

impl FakeStore {
    fn new(state: RecommendationState) -> Self {
        Self {
            state: RefCell::new(state),
            delayed_trigger: RefCell::new(None),
            commits: std::cell::Cell::new(0),
            fail_on_commit: None,
        }
    }

    fn with_delayed_trigger(
        state: RecommendationState,
        loads: usize,
        record: ActivityRecord,
    ) -> Self {
        Self {
            state: RefCell::new(state),
            delayed_trigger: RefCell::new(Some((loads, record))),
            commits: std::cell::Cell::new(0),
            fail_on_commit: None,
        }
    }

    fn failing_commit(state: RecommendationState, fail_on_commit: usize) -> Self {
        Self {
            state: RefCell::new(state),
            delayed_trigger: RefCell::new(None),
            commits: std::cell::Cell::new(0),
            fail_on_commit: Some(fail_on_commit),
        }
    }
}

impl RuntimeStore for FakeStore {
    fn initialize(&self, _now_hour: u64) -> Result<RecommendationState, ()> {
        let ready = {
            let mut delayed = self.delayed_trigger.borrow_mut();
            match delayed.as_mut() {
                Some((remaining, _)) if *remaining > 1 => {
                    *remaining -= 1;
                    None
                }
                Some(_) => delayed.take().map(|(_, record)| record),
                None => None,
            }
        };
        if let Some(record) = ready {
            self.state.borrow_mut().journal.records.push(record);
        }
        Ok(self.state.borrow().clone())
    }

    fn commit(
        &self,
        base: &StateVersion,
        mut next: RecommendationState,
        now_hour: u64,
    ) -> Result<RecommendationState, ()> {
        let commits = self.commits.get() + 1;
        self.commits.set(commits);
        if self.fail_on_commit == Some(commits) {
            return Err(());
        }
        let current = self.state.borrow();
        if StateVersion::of(&current) != *base {
            return Err(());
        }
        next.generation = current.generation + 1;
        next.updated_hour_bucket = now_hour;
        drop(current);
        *self.state.borrow_mut() = next.clone();
        Ok(next)
    }
}

struct FakeDependencies {
    initialize: InitializeResult,
    events: VecDeque<RunnerEvent>,
    writes: std::rc::Rc<RefCell<usize>>,
    spawns: usize,
    sequence: usize,
    waits: usize,
    body_failure: Option<ProcessFailure>,
    cancellation_failed: bool,
    foreground_idle: std::rc::Rc<std::cell::Cell<bool>>,
    foreground_activity_epoch: std::rc::Rc<std::cell::Cell<u64>>,
    become_busy_on_spawn: bool,
    become_busy_then_idle_on_spawn: bool,
    become_busy_before_body_claim: bool,
    body_claimed: std::cell::Cell<bool>,
    body_claim_releases: std::cell::Cell<usize>,
}

impl FakeDependencies {
    fn success() -> Self {
        Self {
            initialize: InitializeResult::Ready {
                model: "main-model".to_string(),
                tools: vec![],
            },
            events: VecDeque::from([
                RunnerEvent::Assistant(
                    r#"{"discarded_activities":[{"activity_id":"activity-1","reason":"no_recommendation_value"}],"recent_tasks":[],"frequent_patterns":[]}"#
                        .to_string(),
                ),
                RunnerEvent::Result { success: true },
                RunnerEvent::End,
            ]),
            writes: std::rc::Rc::new(RefCell::new(0)),
            spawns: 0,
            sequence: 0,
            waits: 0,
            body_failure: None,
            cancellation_failed: false,
            foreground_idle: std::rc::Rc::new(std::cell::Cell::new(true)),
            foreground_activity_epoch: std::rc::Rc::new(std::cell::Cell::new(0)),
            become_busy_on_spawn: false,
            become_busy_then_idle_on_spawn: false,
            become_busy_before_body_claim: false,
            body_claimed: std::cell::Cell::new(false),
            body_claim_releases: std::cell::Cell::new(0),
        }
    }

    fn body_writes(&self) -> usize {
        *self.writes.borrow()
    }

    fn set_foreground_idle(&self, idle: bool) {
        if !idle {
            self.foreground_activity_epoch
                .set(self.foreground_activity_epoch.get() + 1);
        }
        self.foreground_idle.set(idle);
    }
}

impl RuntimeDependencies for FakeDependencies {
    type Process = FakeProcess;

    fn spawn(&mut self, _command: RunnerCommand) -> Result<Self::Process, ProcessFailure> {
        self.spawns += 1;
        if self.become_busy_on_spawn {
            self.set_foreground_idle(false);
        }
        if self.become_busy_then_idle_on_spawn {
            self.set_foreground_idle(false);
            self.set_foreground_idle(true);
        }
        Ok(FakeProcess {
            initialize: self.initialize.clone(),
            events: self.events.clone(),
            writes: self.writes.clone(),
            body_failure: self.body_failure,
            cancellation_failed: self.cancellation_failed,
        })
    }

    fn next_id(&mut self, prefix: &str) -> Result<String, ()> {
        self.sequence += 1;
        static UNIQUE: AtomicUsize = AtomicUsize::new(1);
        Ok(format!(
            "{prefix}-{}-{}",
            self.sequence,
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn process_identity(&self, pid: u32) -> Option<String> {
        Some(format!("identity-{pid}"))
    }

    fn wait_for_writer(&mut self, _duration: Duration) {
        self.waits += 1;
    }

    fn claim_body_write(&self, expected_epoch: u64) -> bool {
        if self.become_busy_before_body_claim {
            self.set_foreground_idle(false);
        }
        let claimed = !self.body_claimed.get()
            && self.foreground_idle.get()
            && self.foreground_activity_epoch.get() == expected_epoch;
        if claimed {
            self.body_claimed.set(true);
        }
        claimed
    }

    fn release_body_write(&self) {
        if self.body_claimed.replace(false) {
            self.body_claim_releases
                .set(self.body_claim_releases.get() + 1);
        }
    }
}

struct FakeProcess {
    initialize: InitializeResult,
    events: VecDeque<RunnerEvent>,
    writes: std::rc::Rc<RefCell<usize>>,
    body_failure: Option<ProcessFailure>,
    cancellation_failed: bool,
}

impl AnalyzerProcess for FakeProcess {
    fn initialize(&mut self, _timeout: Duration) -> Result<InitializeResult, ProcessFailure> {
        Ok(self.initialize.clone())
    }

    fn send_body(&mut self, _body: &str, _timeout: Duration) -> Result<(), ProcessFailure> {
        match self.body_failure.take() {
            Some(ProcessFailure::Transport) => Err(ProcessFailure::Transport),
            Some(failure) => {
                *self.writes.borrow_mut() += 1;
                Err(failure)
            }
            None => {
                *self.writes.borrow_mut() += 1;
                Ok(())
            }
        }
    }

    fn next_event(&mut self, _timeout: Duration) -> Result<RunnerEvent, ProcessFailure> {
        Ok(self.events.pop_front().unwrap_or(RunnerEvent::End))
    }

    fn cancel(&mut self) {}
}

impl RuntimeProcess for FakeProcess {
    fn leader_pid(&self) -> u32 {
        4242
    }

    fn cancellation_failed(&self) -> bool {
        self.cancellation_failed
    }
}

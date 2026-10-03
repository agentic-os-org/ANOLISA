use super::personal_model::*;

#[test]
fn activity_fixture_round_trips_business_entities() {
    let record = ActivityRecord {
        activity_id: "act-1".to_string(),
        session_scope_id: Some("session-opaque".to_string()),
        source_fingerprint: "hmac:v1:event".to_string(),
        observed_hour_bucket: 495_720,
        source: ActivitySource::ShellCommand,
        context: ActivityContext {
            host_id: Some("host:hmac:1".to_string()),
            repo_id: Some("repo:hmac:1".to_string()),
            repo_name: Some("payment".to_string()),
            cwd_relative: Some("services/payment-api".to_string()),
        },
        payload: ActivityPayload::ShellCommand {
            command: "kubectl logs payment-api-abc -n production".to_string(),
            origin: ShellActivityOrigin::Interactive,
            parent_request_activity_id: None,
            outcome: ActivityOutcome::Success,
        },
        redaction: RedactionReport::default(),
        summarized_generation: None,
    };

    let json = serde_json::to_string(&record).expect("serialize activity");
    let decoded: ActivityRecord = serde_json::from_str(&json).expect("deserialize activity");

    assert_eq!(decoded, record);
    assert!(json.contains("payment-api-abc"));
    assert!(json.contains("production"));
    assert!(!json.contains("duration"));
    assert!(!json.contains("hostname"));
}

#[test]
fn analyzer_result_requires_all_arrays_and_rejects_unknown_fields() {
    let valid = r#"{
        "discarded_activities": [],
        "recent_tasks": [],
        "frequent_patterns": []
    }"#;
    assert!(serde_json::from_str::<ProfileAnalyzerResult>(valid).is_ok());
    assert!(serde_json::from_str::<ProfileAnalyzerResult>(
        r#"{"discarded_activities":[],"recent_tasks":[]}"#
    )
    .is_err());
    assert!(serde_json::from_str::<ProfileAnalyzerResult>(
        r#"{"discarded_activities":[],"recent_tasks":[],"frequent_patterns":[],"scope":"host"}"#
    )
    .is_err());
}

#[test]
fn empty_state_has_version_epoch_and_history_baseline() {
    let state = RecommendationState::empty("epoch-1".to_string(), 495_720);

    assert_eq!(state.schema_version, 1);
    assert_eq!(state.store_epoch, "epoch-1");
    assert_eq!(state.generation, 0);
    assert_eq!(state.preferences.user_enabled, None);
    assert_eq!(state.preferences.notice_version_seen, 0);
    assert!(state.journal.records.is_empty());
    assert!(state.journal.history_baseline_pending);
    assert!(state.profile.recent_tasks.is_empty());
    assert!(state.cache.candidates.is_empty());
    assert!(state.scheduler.attempts.is_empty());
}

#[test]
fn recommendation_enablement_prefers_force_off_then_user_choice_then_defaults() {
    let defaults = [
        (None, None, true, true),
        (None, None, false, false),
        (Some(true), None, false, true),
        (Some(false), None, true, false),
        (Some(true), Some(false), true, false),
        (Some(false), Some(true), false, false),
        (None, Some(true), false, true),
    ];

    for (environment, preference, configured, expected) in defaults {
        assert_eq!(
            resolve_recommendations_enabled(environment, preference, configured),
            expected
        );
    }
}

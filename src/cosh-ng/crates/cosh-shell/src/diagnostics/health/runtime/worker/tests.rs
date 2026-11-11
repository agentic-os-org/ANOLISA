use super::*;

#[test]
fn actual_worker_fixture_uses_the_selected_canonical_plan() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let request = super::super::plan::WorkerRequest {
        scan_id: "health-1".into(),
        started_at_ms: 1,
        now_epoch_seconds: 100,
        deadline: JobDeadline::after(Duration::from_secs(3)).unwrap(),
        context: ScanContext::Resources,
        config: crate::config::HealthConfig::default(),
        environment: None,
        disabled: vec!["resource.memory".into()],
        config_source: super::super::plan::ConfigSource::CallerSnapshot,
        fixture_name: None,
    };
    let mut wire = serde_json::to_value(request).unwrap();
    wire.as_object_mut().unwrap().insert(
        "fixture_name".into(),
        serde_json::Value::String("linux-warning".into()),
    );
    let request = serde_json::from_value(wire).expect("same worker accepts a fixture request");
    let report = super::super::scan::HealthScanHandle::start(request)
        .finish_within(Duration::from_secs(3))
        .unwrap();
    assert!(!report.facts.is_empty());
    assert!(report
        .facts
        .iter()
        .all(|fact| fact.source == super::super::super::model::HealthFactSource::Fixture));
    assert!(!report
        .facts
        .iter()
        .any(|fact| fact.key == "memory.available_mib"));
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "memory.swap_used_ratio"));
    assert!(report.try_items.iter().all(|item| report
        .findings
        .iter()
        .any(|finding| finding.id == item.finding_id)));
}

#[test]
#[ignore = "same-package worker entry used by the unit launcher"]
fn private_worker_child() {
    std::process::exit(run_private_health_worker());
}

#[test]
fn environment_snapshot_contains_only_diagnostic_inputs() {
    let config = crate::config::CoshConfig::default();
    let snapshot =
        super::super::plan::EnvironmentSnapshot::capture(&config, std::path::Path::new("/tmp"));
    let encoded = serde_json::to_string(&snapshot).expect("diagnostic snapshot");
    assert!(!encoded.contains("readonly"));
    assert!(!encoded.contains("trusted_commands"));
    assert!(!encoded.contains("api_key"));
    assert!(!encoded.contains("access_key"));
}

#[test]
fn environment_snapshot_preserves_the_callers_approval_fact() {
    let mut config = crate::config::CoshConfig::default();
    config.approval_mode = crate::types::CoshApprovalMode::Trust;
    let snapshot =
        super::super::plan::EnvironmentSnapshot::capture(&config, std::path::Path::new("/tmp"));
    let rebuilt = snapshot
        .config(config.health.clone())
        .expect("valid snapshot");
    assert_eq!(rebuilt.approval_mode, config.approval_mode);
}

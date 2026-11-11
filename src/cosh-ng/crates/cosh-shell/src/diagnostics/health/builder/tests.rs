use super::*;

#[test]
fn generated_scan_id_is_stable_and_local() {
    assert_eq!(health_scan_id(12345), "health-12345");
}

#[test]
fn builder_assigns_fact_ids_and_keeps_partial_results() {
    let mut builder = HealthReportBuilder::for_started_at(100);
    builder
        .set_host("db-1")
        .set_role(Some("mysql-primary".to_string()))
        .set_health_score(Some(73))
        .add_check_done("memory")
        .add_fact(
            HealthFactCategory::Memory,
            "memory.available_mib",
            HealthFactValue::Unsigned(700),
            Some("MiB".to_string()),
            HealthFactSource::ProcMeminfo,
            4,
        )
        .add_fact(
            HealthFactCategory::Memory,
            "memory.available_mib",
            HealthFactValue::Unsigned(690),
            Some("MiB".to_string()),
            HealthFactSource::Derived,
            5,
        )
        .add_unavailable(
            HealthCollector::KernelSignal,
            HealthUnavailableReason::PermissionDenied,
            HealthSeverity::Unavailable,
            7,
        );

    let report = builder.finish(150);

    assert_eq!(report.scan_id, "health-100");
    assert_eq!(report.host.as_deref(), Some("db-1"));
    assert_eq!(report.role.as_deref(), Some("mysql-primary"));
    assert_eq!(report.health_score, Some(73));
    assert_eq!(report.elapsed_ms, 50);
    assert_eq!(report.checks_done, vec!["memory"]);
    assert_eq!(report.facts[0].id, "memory.available_mib");
    assert_eq!(report.facts[1].id, "memory.available_mib#2");
    assert_eq!(report.unavailable.len(), 1);
    assert_eq!(report.overall_severity, HealthSeverity::Unavailable);
}

#[test]
fn finish_uses_saturating_elapsed_time() {
    let report = HealthReportBuilder::for_started_at(200).finish(150);

    assert_eq!(report.elapsed_ms, 0);
}

#[test]
fn incomplete_check_is_not_projected_as_done() {
    let mut builder = HealthReportBuilder::for_started_at(100);
    builder
        .add_check_done("memory")
        .add_fact(
            HealthFactCategory::Memory,
            "memory.available_mib",
            HealthFactValue::Unsigned(700),
            Some("MiB".to_string()),
            HealthFactSource::ProcMeminfo,
            1,
        )
        .add_unavailable(
            HealthCollector::Memory,
            HealthUnavailableReason::Timeout,
            HealthSeverity::Degraded,
            10,
        );
    let report = builder.finish(110);
    assert!(!report.checks_done.contains(&"memory".to_string()));
    assert_eq!(report.facts.len(), 1);
}

#[test]
fn streamed_partial_facts_keep_metadata_and_disabled_checks() {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = events.clone();
    let mut worker = HealthReportBuilder::for_started_at(1000).with_progress(move |event| {
        recorded.lock().expect("progress lock").push(event);
        true
    });
    worker
        .start_check(
            HealthCollector::Memory,
            HealthFactScope::Host,
            HealthFactCoverage::Snapshot,
            1000,
        )
        .add_fact(
            HealthFactCategory::Memory,
            "memory.available_mib",
            HealthFactValue::Unsigned(700),
            Some("MiB".to_string()),
            HealthFactSource::ProcMeminfo,
            4,
        );
    let mut parent = HealthReportBuilder::for_started_at(1000);
    parent
        .declare_check(HealthCollector::Memory, true)
        .declare_check(HealthCollector::Provider, false);
    for event in events.lock().expect("progress lock").iter().cloned() {
        parent.apply_progress(event).expect("same model progress");
    }
    parent.seal_pending(HealthUnavailableReason::Timeout, 50);
    let report = parent.finish(1050);
    let memory = report
        .check_results
        .iter()
        .find(|check| check.id == "memory")
        .expect("memory check");
    assert_eq!(memory.status, HealthCheckStatus::Partial);
    assert_eq!(memory.elapsed_ms, 50);
    assert_eq!(
        report.fact_metadata["memory.available_mib"].sampled_at_ms,
        1004
    );
    assert_eq!(
        report.fact_metadata["memory.available_mib"].scope,
        HealthFactScope::Host
    );
    assert!(report.checks_done.is_empty());
    assert_eq!(report.unavailable.len(), 1);
    assert_eq!(report.resource_severity, HealthSeverity::Ok);
    assert_eq!(
        report
            .check_results
            .iter()
            .find(|check| check.id == "provider")
            .expect("disabled check")
            .status,
        HealthCheckStatus::Disabled
    );
    let encoded = serde_json::to_vec(&report).expect("report wire model");
    assert_eq!(
        serde_json::from_slice::<HealthScanReport>(&encoded).expect("same report model"),
        report
    );
}

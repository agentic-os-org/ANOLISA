use super::*;

fn options() -> HealthScanOptions {
    HealthScanOptions {
        started_at_ms: 1,
        now_epoch_seconds: 100,
        total_budget: Duration::from_secs(1),
        mode: HealthScanMode::Fixture("linux-critical".into()),
    }
}

#[test]
fn disabled_doctor_fixture_uses_the_same_registry_without_fixture_success() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let previous = std::env::var_os(HEALTH_SCAN_ENV);
    std::env::set_var(HEALTH_SCAN_ENV, "fixture:linux-critical");
    let mut config = crate::config::CoshConfig::default();
    config.health.enabled = false;
    let report = run_supervised_doctor_report(&config, std::path::Path::new("/tmp"));
    match previous {
        Some(value) => std::env::set_var(HEALTH_SCAN_ENV, value),
        None => std::env::remove_var(HEALTH_SCAN_ENV),
    }
    assert_eq!(report.check_results.len(), health_check_ids().len());
    assert!(report
        .check_results
        .iter()
        .all(|check| check.status == super::super::super::model::HealthCheckStatus::Disabled));
    assert!(report.checks_done.is_empty());
    assert!(report.health_score.is_none());
    assert!(report.facts.is_empty() && report.findings.is_empty());
}

#[test]
fn disabling_root_does_not_relabel_its_legacy_summary_as_an_extra_mount() {
    let report = fixture_report_for_context(
        "linux-critical",
        &HealthConfig::default(),
        &options(),
        plan::ScanContext::Resources,
        &["filesystem.root".into()],
    );
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.id.starts_with("J09") || finding.id.starts_with("J10")),
        "disabled root cannot produce a disk warning through mounts: {report:?}"
    );
}

#[test]
fn memory_swap_switch_matrix_keeps_fact_ownership_and_references() {
    for context in [
        plan::ScanContext::Resources,
        plan::ScanContext::LoginBanner,
        plan::ScanContext::Startup,
        plan::ScanContext::OnDemand,
    ] {
        for memory in [false, true] {
            for swap in [false, true] {
                let mut config = HealthConfig::default();
                config.checks.insert("resource.memory".into(), memory);
                config.checks.insert("resource.swap".into(), swap);
                let report =
                    fixture_report_for_context("linux-critical", &config, &options(), context, &[]);
                for (id, enabled) in [("resource.memory", memory), ("resource.swap", swap)] {
                    let check = report
                        .check_results
                        .iter()
                        .find(|check| check.id == id)
                        .unwrap();
                    assert_eq!(
                        check.status == super::super::super::model::HealthCheckStatus::Disabled,
                        !enabled
                    );
                    if !enabled {
                        assert!(check.fact_ids.is_empty());
                    }
                }
                assert_eq!(
                    report
                        .facts
                        .iter()
                        .any(|fact| fact.key == "memory.available_mib"),
                    memory
                );
                assert_eq!(
                    report
                        .facts
                        .iter()
                        .any(|fact| fact.key == "memory.swap_used_mib"),
                    swap
                );
                assert!(report
                    .findings
                    .iter()
                    .flat_map(|finding| &finding.evidence_fact_ids)
                    .all(|id| report.facts.iter().any(|fact| fact.id == *id)));
            }
        }
    }
}

#[test]
fn equal_resource_evidence_uses_equal_rules_and_candidates_across_entries() {
    for name in ["linux-healthy", "linux-warning", "linux-critical"] {
        let baseline = fixture_report_for_context(
            name,
            &HealthConfig::default(),
            &options(),
            plan::ScanContext::Resources,
            &[],
        );
        for context in [
            plan::ScanContext::LoginBanner,
            plan::ScanContext::Startup,
            plan::ScanContext::OnDemand,
        ] {
            let report = fixture_report_for_context(
                name,
                &HealthConfig::default(),
                &options(),
                context,
                &[],
            );
            assert_eq!(report.resource_severity, baseline.resource_severity);
            assert_eq!(report.findings, baseline.findings);
            assert_eq!(report.try_items, baseline.try_items);
        }
    }
}

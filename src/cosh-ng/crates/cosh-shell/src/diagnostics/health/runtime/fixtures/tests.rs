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

#[test]
fn pressure_fixture_gates_notices_and_signals_recovery() {
    let config = HealthConfig {
        pressure_notices: true,
        ..HealthConfig::default()
    };
    let report = fixture_report("linux-pressure", &config, &options());
    let mut ids: Vec<String> = report
        .findings
        .iter()
        .map(|finding| finding.id.clone())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["J19", "J20", "J20:cgroup", "J21", "J22"]);
    assert!(report
        .findings
        .iter()
        .all(|finding| finding.severity == HealthSeverity::Warning));
    let off = fixture_report("linux-pressure", &HealthConfig::default(), &options());
    assert!(off.findings.is_empty());
    let low = fixture_report("linux-pressure-low", &config, &options());
    assert!(low.findings.is_empty());
    let entry = crate::diagnostics::health::suppression::episode::signals_from_report(&report);
    assert!(entry
        .quota
        .iter()
        .any(|combo| combo.key == "cgroup|7:77|quota" && combo.max.entry));
    assert!(entry
        .volume
        .iter()
        .any(|member| member.key == "host|psi_memory" && member.entered));
    let exit = crate::diagnostics::health::suppression::episode::signals_from_report(&low);
    assert!(exit
        .quota
        .iter()
        .any(|combo| combo.psi_recovery && combo.max.exit && combo.high.exit));
    assert!(exit
        .volume
        .iter()
        .any(|member| member.key == "host|psi_memory" && member.exit && member.valid));
}

#[test]
fn conntrack_fixture_gates_notices_and_keeps_d_facts_silent() {
    let config = HealthConfig {
        network_capacity_notices: true,
        ..HealthConfig::default()
    };
    let report = fixture_report("linux-conntrack", &config, &options());
    let ids: Vec<&str> = report
        .findings
        .iter()
        .map(|finding| finding.id.as_str())
        .collect();
    assert_eq!(ids, vec!["J23"]);
    assert_eq!(report.findings[0].severity, HealthSeverity::Warning);
    let off = fixture_report("linux-conntrack", &HealthConfig::default(), &options());
    assert!(off.findings.is_empty());
    let low = fixture_report("linux-conntrack-low", &config, &options());
    assert!(low.findings.is_empty());
    let zero = fixture_report("linux-conntrack-zero", &config, &options());
    assert!(zero.findings.is_empty());
    let entry = crate::diagnostics::health::suppression::episode::signals_from_report(&report);
    assert!(entry
        .volume
        .iter()
        .any(|member| member.key == "host|conntrack" && member.entered));
    let exit = crate::diagnostics::health::suppression::episode::signals_from_report(&low);
    assert!(exit
        .volume
        .iter()
        .any(|member| member.key == "host|conntrack" && member.exit));
    let removed = crate::diagnostics::health::suppression::episode::signals_from_report(&zero);
    assert!(removed
        .volume
        .iter()
        .any(|member| member.key == "host|conntrack" && member.exit && !member.entered));
}

#[test]
fn pids_fixture_stays_diagnostics_only() {
    let config = HealthConfig {
        network_capacity_notices: true,
        pressure_notices: true,
        ..HealthConfig::default()
    };
    let report = fixture_report("linux-pids", &config, &options());
    assert!(report.findings.is_empty());
    assert!(report
        .facts
        .iter()
        .any(|fact| fact.key == "kernel.cgroup_pids_level"));
    let signal = crate::diagnostics::health::suppression::episode::signals_from_report(&report);
    assert!(signal.pids_counter.is_none());
}

#[test]
fn fs_events_fixture_emits_proven_chain_findings() {
    let report = fixture_report("linux-fs-events", &HealthConfig::default(), &options());
    let mut found: Vec<(String, HealthSeverity)> = report
        .findings
        .iter()
        .map(|finding| (finding.id.clone(), finding.severity))
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            ("J24".into(), HealthSeverity::Critical),
            ("J29".into(), HealthSeverity::Warning)
        ]
    );
    let candidate = crate::diagnostics::health::suppression::episode::candidate_for_finding(
        &report, "J24", 4, "J24:/",
    )
    .unwrap();
    assert!(candidate.event);
    assert_eq!(
        candidate.key,
        "fs-event-v1|boot-a|ext4-forced-ro|sda1|uptime-ns:1099000000000"
    );
    let block = crate::diagnostics::health::suppression::episode::candidate_for_finding(
        &report, "J29", 3, "J29:/",
    )
    .unwrap();
    assert_eq!(block.problem, "block-io|8:1");
}
